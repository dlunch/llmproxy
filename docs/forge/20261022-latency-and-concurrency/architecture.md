# Request latency and provider concurrency architecture

## Scope and structure

Backend change within the existing API → Gateway → Provider/PgStore boundaries. PostgreSQL remains authoritative across replicas. No request cache, subprocess pool, or inference retry layer is added. Backend has no matching Forge supplemental guideline. CLI-tool guidance applies only to preserving JSON stdout and graceful process shutdown.

```
API: start clock → authenticate → validate
Gateway: parallel routing reads → record request → provider → events → atomic finalization
Provider: read credentials → short coordinated refresh when needed → independent inference
Logging: existing JSON formatter → bounded library queue → dedicated stdout worker
```

## Persisted usage and atomic quota writes

Add `0002_usage_totals.sql`; leave `0001_initial.sql` unchanged. Add `input_tokens` and `output_tokens` bigint columns, nonnegative and default zero, to accounts and provider_instances. Within the migration transaction, lock requests against writers before ALTERing either parent table and before taking the backfill snapshot; initialize both totals from existing request rows, preserving nullable/known counts exactly. Install a compact PostgreSQL trigger in the same transaction so every subsequent request insert, token-count update, or deletion maintains those totals, including deployment overlap. There is no old/new application fallback.

Keep `PgStore::account_usage`, `provider_states`, and `finish_request` signatures. Reads select totals from their owning rows, with existing account isolation. Keep the existing conditional `UPDATE requests ... WHERE outcome='running'` finalization gate. An AFTER INSERT/UPDATE OF input_tokens,output_tokens/DELETE trigger computes the input/output count delta using NULL as zero, skips zero deltas, then increments the owning account followed by its provider. PostgreSQL commits the request mutation and both totals atomically. Count known usage on every terminal outcome. Repeated finalization changes no row and therefore no totals. Request ownership is immutable in current runtime paths; do not add speculative reassignment machinery. Reconciliation changes no token columns and requires no accounting special case. Historical statistics view remains an explicitly historical query; routing never uses it.

`save_quota` keeps its interface and uses one bound multirow INSERT (SQLx QueryBuilder), with its existing observation-time conflict guard. Empty input does no I/O. Sort borrowed window references by key before binding values so concurrent writers acquire quota rows in the same order; this avoids opposite-order deadlocks from Claude HashMap iteration. Internal producers already provide unique window keys. Statement atomicity replaces explicit begin/per-window/commit; invalid data cannot partially update windows.

Tradeoff: bounded reads replace history scans with two short contended counter-row writes per completion. Account-level completion serialization is measured under load. The one-time migration scans history and blocks writers; report its scale separately from steady-state latency.

## Credential and provider ownership

Add unlocked `PgStore::codex_credentials(provider_id)` and `claude_credentials(provider_id)` returning existing credential structs. Existing lock/save methods remain the authority for refresh. Fresh reads avoid BEGIN/FOR UPDATE/COMMIT. A due credential acquires the row lock, rereads, and checks freshness again. Maintenance retains SKIP LOCKED. Refresh HTTP and persistence run in a tracked task once they can rotate credentials; cancellation of the caller stops waiting without aborting that task. Shutdown drains tracked refresh work.

Codex reuses `refresh_locked_credentials`; its fresh-token path proceeds directly to the existing HTTP call. No new forced retry behavior is added.

Claude inference cannot safely run multiple writable credential-file snapshots. The host becomes the sole refresh-token owner. Pass the access token through the official OAuth token channel, retain isolated per-request config/history/tool state, and never expose refresh tokens to children or write child auth snapshots back to PostgreSQL. Account/onboarding metadata is initialized from the authoritative row and remains host-owned. Remove the obsolete auth-file readback path and its tests.

Preserve mid-session expiration recovery through the official SDK `oauth_token_refresh` control exchange in the existing stdin/stdout control pump. Track the current access token for that child. On a refresh request, lock/reload the authoritative row; if its access token differs from the child's rejected token, reuse the newer token; otherwise force one tracked host refresh and persist before replying with `accessToken`. Update the child's tracked token after a successful reply. Concurrent requests and maintenance therefore deduplicate without holding a lock during inference. Never replay an inference request to implement refresh. A refresh error becomes a redacted provider error; a rotated credential must still commit after child cancellation. Quota finalization remains in the tracked inference worker.

**Verified protocol:** unmodified official CLI 2.1.307 was exercised locally with `CLAUDE_CODE_ENTRYPOINT=local-agent`, `CLAUDE_CODE_SDK_HAS_OAUTH_REFRESH=1`, and a dummy OAuth token. A local 401 produced `control_request.request.subtype=oauth_token_refresh`; a standard success control_response with `response:{accessToken:...}` retried with the new bearer token, returned text/result success and quota. Preserve this exchange in the repository integration test. The CLI callback timeout is 30 seconds; timeout/cancellation may stop inference but must not stop persistence of an already-started rotation. Plain static environment-token mode is insufficient.

## Logging and lifecycle

Use `tracing-appender` with `NonBlockingBuilder::lossy(false)` and a fixed bounded queue (256 records), retaining the current JSON subscriber and complete payload fields. A standard `BufWriter` inside the sink wrapper combines small writes until the library's batch-end flush. Formatting stays on the emitting thread; sink writes move to a dedicated worker. A full queue blocks producers, including Tokio workers. Queue capacity bounds record count, not bytes; maximum request/output sizes still determine worst-case memory. Do not promise nonblocking behavior under saturation.

The library guard alone has a bounded shutdown timeout. Add a minimal sink wrapper and explicit flush acknowledgment: after HTTP/provider/maintenance work stops producing logs and the final shutdown/error record is emitted, enqueue a reserved raw-NUL marker through the same NonBlocking writer. The worker sink recognizes that exact byte sequence, flushes stdout and sends a one-shot result instead of emitting the marker. Raw NUL cannot occur in the subscriber's valid JSON records (JSON escapes it); the library preserves each queued write when calling its sink. Await acknowledgment before dropping WorkerGuard. This requires no per-record bookkeeping or custom queue. Verify the exact library write behavior in the pinned version and test JSON bodies containing NUL. A functioning slow sink must finish draining; the sink retains its first write/flush error and returns that error at the barrier even if the final flush succeeds. This is necessary because the library worker continues after write errors. Do not use process::exit before drain; return ExitCode normally.

Source: https://docs.rs/tracing-appender/latest/tracing_appender/non_blocking/struct.NonBlockingBuilder.html and https://docs.rs/tracing-appender/latest/src/tracing_appender/non_blocking.rs.html .

## Stage metrics and timing contracts

Use the shared `Arc<Metrics>` in provider constructors; no frontend dependency enters providers. Introduce a finite `Stage` enum and `Metrics::observe_stage(stage, Duration)`. Histogram labels use only finite stage names (and, if needed, existing configured provider IDs), never token/account/request IDs or model input. Buckets include 1, 2.5, 5, 10, 25, 50, 100, 250, 500 ms and existing multi-second tails.

Start an Instant in each inference API handler before authenticate; measure authentication around its actual result, including failures. Pass the originating Instant into `Gateway::execute` as an explicit argument. Routing measures parallel reads plus selection. Provider preparation measures `infer` until it returns its stream; providers separately measure credential preparation including read, lock wait, and refresh. These are intentionally overlapping stage durations, not additive breakdowns. First-text measures from the API origin until the gateway receives the first nonempty TextDelta; tool events, usage events, role/metadata frames, and empty deltas do not qualify. Tool-only or pre-text failures have no first-text observation. Request total uses the same origin through durable terminal accounting, including auth/routing. Document that it excludes framework body extraction before handler entry and final client network drain; it is an admitted inference lifecycle duration. Preserve existing request/token/error counters.

Periodic maintenance uses separate `maintenance_credential_lock` and `maintenance_credential_refresh` stage labels. Its skipped-lock checks do not dilute the request `credential_lock` / `credential_refresh` distributions. Only actual exchanges contribute refresh durations, including persistence and failures.

Auth/routing/credential/preparation timers must record completed error paths as well as success. Extend controlled-delay fixtures to prove boundaries and avoid mislabeled first-frame metrics. Do not expand this change into a general tracing framework.

## Implementation blocks and ownership

| Order | Responsibility | Core | Owner/files | Dependencies |
|---|---|---|---|---|
| 1 | Bounded usage reads, atomic finalization/quota, credential read methods | yes | architecture: store.rs, migration, gateway integration tests | none |
| 2 | Worker logging and drain | yes | root: main.rs, logging module, Cargo files/tests | none |
| 3 | Credential ownership and provider concurrency | yes | provider_research: both providers and provider tests | store credential read methods; official protocol evidence |
| 4 | Stage timing and lifecycle propagation | yes | root: metrics.rs, api.rs, gateway.rs, API tests; provider owner wires injection | agreed Metrics API; provider implementation |
| 5 | Integrated performance comparison and documentation | yes | root with review agent | all blocks |

Blocks 1 and 2 can proceed independently; provider work starts after store signatures and metrics API are agreed. Shared-file edits remain with the named owner. Each core block receives implementation review before full QA. Existing checkout is retained.

## Verification

- Extend PostgreSQL gateway tests: migrate a populated 0001 schema, verify totals; successful/failed/cancelled known usage; duplicate and concurrent finalization; forced counter update failure rolls back request and both totals; account isolation and routing selection; quota multiwindow rollback and older-observation protection. Direct inserts/updates/deletes must preserve the same totals as historical SUM semantics, including nullable counts; the trigger is the single accounting writer.
- Extend Codex tests: fresh request progresses while another transaction holds credential row lock; concurrent expired request/maintenance refresh once; cancellation after refresh begins still persists; quota ordering and streaming remain intact.
- Extend Claude tests: two same-provider processes reach a barrier concurrently; no child refresh token; concurrent stale-token control requests refresh once; newer stored token reused; cancellation during refresh commits; persistence errors surface; dropped stream reaps process; quotas and tools persist. Remove serialized-child-rotation assumptions. Run official CLI locally for ordinary inference/tools/quotas and a forced 401 refresh exchange.
- Logging tests use a controlled slow writer: queue saturation blocks rather than drops, all JSON records survive including bodies, and explicit drain waits beyond the library guard's built-in timeout. Exercise sink failure and normal exit behavior.
- API fixtures cover both surfaces with delayed auth/routing/provider/text, empty deltas, tool-only output and errors. Assert histogram counts/boundaries and total includes pre-gateway work.
- Run fmt, clippy, complete tests, and official CLI test. Release measurements compare actual base 2d13928 to final changes using identical local fixtures, warmed runs, concurrency 1 and multiple concurrent requests, small and large history (at least 100k finalized requests), and realistic full logging to a functioning sink. Report request/first-text percentiles, throughput, query plans/cost, same-provider Claude overlap, queue saturation limits and fixture constraints. Do not extrapolate local latency to paid upstream inference.
