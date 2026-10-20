# Core and Claude implementation review

What this change does: Accounts authenticate through owned bearer tokens or CIDR ranges. The gateway selects a subscription provider, streams normalized events, and records account usage and request outcomes. Claude is isolated in a temporary working directory with an explicit tool allowlist and a client-function bridge.

Reviewed `core.rs`, `store.rs`, `gateway.rs`, the schema and gateway integration test; independently reviewed `providers/claude.rs` and its current subprocess fixture. Also followed cancellation into the connected Codex credential task. The core implementation review is a self-review; the Claude/Codex findings are independent review findings.

## Findings resolved during implementation

1. **High: Claude stops before the upstream assistant message is complete** (`src/providers/claude.rs`, assistant branch and immediate function handoff).
   - The real CLI emits an `assistant` record for an individual completed content block, before the subsequent `message_delta` and `message_stop` stream events. This ordering is visible in the already captured CLI mock output.
   - Killing the process at the first function-bearing assistant record loses any later parallel function calls in the same response and may use the initial, incomplete output-token count. A response containing two client function blocks therefore returns only the first.
   - Collect calls through the upstream message boundary and obtain the final usage update before handoff. Pending client `tools/call` control requests must remain unexecuted while the message finishes. Add a fixture using the real per-block ordering with two calls and a final usage delta.

2. **High: Search followed by a function handoff loses earlier usage** (`src/providers/claude.rs`, usage emitted inside function handoff).
   - The provider ignores earlier assistant-message usage and emits only the current message's usage when a client function is requested. No final CLI aggregate result is reached because the process is intentionally stopped.
   - A web-search assistant turn using 10 input/5 output tokens followed by a function-call turn using 20 input/5 output tokens records 20/5 instead of 30/10. Account routing and history then undercount actual observed usage.
   - Maintain cumulative usage for distinct upstream message IDs, replacing updates for the same message rather than summing repeated per-block records. Emit that total at the handoff; use final CLI aggregate usage for ordinary completion. Cover the search-to-function sequence.

3. **High: Cancelled Codex requests leave an unbounded queue of credential tasks** (`src/providers/codex.rs`, refresh task creation before mutex acquisition).
   - Every request spawns a detached credential task which then waits on the provider mutex. Cancelling the gateway request releases its concurrency permit but does not remove that queued task. Repeated cancellations during a slow refresh can therefore accumulate work beyond the configured request concurrency bound.
   - Acquire an owned mutex guard in the inference future before spawning the durable task, then move the guard into that task. Waiting callers remain cancellable; at most one credential task per provider survives cancellation to persist a rotated token.
   - The credential-task persistence behavior itself is necessary and should be retained. The finding concerns queued detached tasks, not the durability requirement.

## Core/account review outcome

No additional blocker found in the current core/store/gateway paths. Token and IP lookups join enabled accounts; explicit invalid bearer credentials never fall back to IP authentication. IP matching uses the direct canonical TCP peer and PostgreSQL longest-prefix matching. Request records retain account ownership and one credential source. Usage sums use account IDs across all owned credentials, and provider totals have no lost-update counter race.

Provider setup and streaming are both inside the gateway cancellation/deadline scope. The task owns accounting after downstream disconnect. It requires a provider terminal event, preserves observed partial usage, bounds aggregate payload strings, and persists completion before forwarding success. Database failure cannot silently produce success. Expired unfinished requests are reconciled after a grace period.

Claude's configured built-in tool list contains only requested WebSearch; client functions are exposed through the controlled SDK MCP bridge rather than executed as host commands. Argument passing avoids shell interpolation, stdin carries user content, and temporary history uses JSON serialization. `kill_on_drop` is retained for dropped provider futures/streams.

## Load impact

Each accepted request adds authentication lookup, two provider-state reads, one account-usage aggregate, one lifecycle insert, and one terminal update. Quota observations add a small transaction of per-window upserts. Account and provider lookup columns are indexed. Aggregates scan matching historical rows; no throughput claim was measured. Current deployment assumes one process for provider refresh serialization, as documented.

## Verification and limits

The real PostgreSQL gateway integration test passed after account/IP/tool changes (one integration test, 1.60 seconds). It covers account totals shared across token/IP credentials, account disablement, longest-prefix routing, invalid tokens, multi-window provider quota, concurrent totals, provider setup timeout and cancellation, truncated upstream output, persistence failures, output-size bounds, reconciliation, and metrics.

The three findings were corrected: Claude waits for the upstream message boundary and tracks cumulative per-message usage; Codex obtains its owned refresh mutex guard before detaching persistence work. The expanded Claude fixtures pass: four tests cover terminal/exit failures, parallel handoff and cumulative native-search usage, result history fidelity, and explicit cancellation or direct stream drop. The parallel fixture uses real CLI per-content-block ordering and asserts both calls and final usage (35 input/12 output tokens).

An additional high-impact defect was found by running installed Claude 2.1.302 against a local fake Messages API: supplying tool results on stdin after resuming an unfinished assistant history caused the CLI to repair the tool calls with error results before consuming stdin. The fix includes the final tool-result user row in resume history and uses the documented `CLAUDE_CODE_RESUME_INTERRUPTED_TURN=1` continuation behavior, with no extra user input. Text-block result arrays keep CLI-added reminders separate. The opt-in real CLI roundtrip test passed (one test, 2.10 seconds), verifying two calls and both original result payloads in a three-turn transcript; only the native trailing newline and separate reminder blocks are allowed.

Targeted commands: `cargo test --test claude_provider` passed four tests with the real CLI test ignored by default; `CLAUDE_TEST_EXECUTABLE=/usr/bin/claude cargo test --test claude_provider actual_cli_roundtrips -- --ignored --nocapture` passed the installed-CLI fixture. Final whole-project QA belongs to the separate QA report. Real subscription calls and real OAuth rotation were not tested.

Verdict: these findings are resolved and targeted Claude verification passes.
