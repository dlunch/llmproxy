# QA report

Final result: PASS with the measured performance tradeoffs documented below. Functional verification passed: 24 distinct tests, 0 failed, 0 ignored, 1 manual benchmark filtered. No unresolved correctness/security findings remain.

## Coverage against the architecture

| Requirement | Observable verification | Result |
|---|---|---|
| Preserve historical usage during migration | `usage_migration_and_mutations_preserve_atomic_totals` starts from populated migration 0001, applies current migrations, and checks known/nullable counts | PASS |
| Atomic totals across finalization, direct mutations and deletion | Same test covers insert/update/delete, duplicate concurrent finalization, and forced provider-counter failure rolling back request/account/provider state | PASS |
| Failed/cancelled usage and account isolation | Extended `postgres_gateway_lifecycle_routing_and_cancellation` plus existing API account tests | PASS |
| Atomic quota snapshots and observation order | Gateway tests cover invalid multiwindow rollback, stale snapshots, empty input, and concurrent reversed-key submissions | PASS |
| Fresh credential reads avoid exclusive waits | Codex and Claude integration fixtures hold the credential row lock in a separate connection while fresh inference completes | PASS |
| Refresh ownership survives races and cancellation | Existing Codex cancellation scenario and `oauth_callbacks_coordinate_replicas_and_persist_after_cancellation` | PASS |
| Concurrent same-provider Claude inference | `concurrent_calls_share_credentials_and_persist_quota_independently` requires both subprocesses to reach a barrier; fixtures use one-connection pools | PASS |
| Official CLI 401 recovery, tools and quota | Actual unmodified CLI 2.1.307 against local fake API, with host OAuth refresh and persisted rotated token | PASS |
| No child refresh-token ownership | Synthetic subprocess verifies absence of credential file and presence of the official access-token/control configuration | PASS |
| Logging saturation and slow drain | Controlled writer tests fill the bounded queue, prove producer backpressure, retain all records, escape JSON NUL and wait beyond the library guard timeout | PASS |
| Sink error reporting and interrupted writes | Existing writer test covers write/zero-progress/flush errors and successful retry after interruption | PASS |
| Full correlated logs without authentication secrets | Isolated `json_logs_include_correlated_bodies_and_never_authentication_headers` uses the actual worker/drain and preserves all assertions | PASS |
| Authentication/routing/first-text timing boundaries | `latency_metrics_include_database_waits_and_first_nonempty_text` injects database waits and provider/text delays on both API surfaces; empty/usage/tool-only/error paths do not count as first text | PASS |
| Provider error/cancellation stage observations | Extended provider fixtures assert credential read/lock/refresh metrics and maintenance separation | PASS |
| Representative release comparison | Four serial ABBA runs against base `2d13928`; raw output, CSV and table calculations independently checked | PASS |

## Test quality and independent execution

The tests exercise PostgreSQL transactions, actual subprocesses, local HTTP exchanges, externally observable stream outcomes and sink behavior. Test doubles retain the real provider logic; the official CLI fixture independently confirms the supported control protocol. Database constraint/trigger failures establish rollback behavior instead of asserting only successful paths. Existing fixtures were extended; no test-only production interface was introduced.

The first independent full run caught a scoped tracing subscriber/callsite-interest race: the logging fixture captured only three of five request records during parallel API execution. The fixture was isolated in a child test process with stable global dispatch, without weakening assertions. The complete suite then passed. The child invocation executes the same logging test and is not counted twice in the 24-test total.

Executed commands:

- `cargo test --locked --all-targets -- --include-ignored --skip proxy_latency_benchmark` with local `TEST_DATABASE_URL` and `CLAUDE_TEST_EXECUTABLE`: passed. Counts: logging unit 2, API 8, Claude 8, Codex 2, configuration 1, maintenance 1, gateway 2.
- `cargo fmt --all -- --check`: passed after applying formatting.
- `cargo clippy --locked --all-targets -- -D warnings`: passed.
- `git diff --check`: passed.

After the full-suite pass, standard sink buffering was added inside the existing drain/error wrapper. Read-only review confirmed that both batch flushes and the explicit barrier still flush the buffer and report errors. The implementing agent reported successful targeted revalidation on this final logger: 2 logger unit tests, 1 isolated API logging integration test, formatting, locked Clippy and whitespace checks. These targeted reruns are not added to the distinct-test count above.

## Performance evidence review

Independently checked all 96 per-run scenario summaries in `latency-samples.csv` against the four raw run outputs, all 24 displayed table rows against the arithmetic mean of their two per-version runs, and all reported percentage ranges. Each scenario had the stated 50 or 200 measured requests. The baseline runtime, build and migration files matched `2d13928`; the benchmark workload/measurement logic matched the final harness, with logger wiring/drain as the intended difference. Formatting differences do not change the fixture.

The mean per-run total p50 improved by 90.2–90.7% with 100,000 seeded requests at concurrency one and 81.1–81.7% at concurrency four. Short responses without seeded history regressed by 3.3–8.0% and 20.7–28.4%, respectively. Large-stream p50 regressed by 15.2–17.4% and approximately 28%. These are final measured tradeoffs, not unresolved correctness failures or evidence of a universal speedup. No fixed global speedup threshold was part of the accepted PRD.

The query-level measurement separately demonstrates removal of history scans, with its planning, warm-buffer and one-time migration limits stated. Concurrent API runs include counter-write contention but are not a saturation or multi-replica capacity test. The final report and work-history record accurately preserve both improvements and regressions. No performance acceptance item remains pending.

## Limits

Local fake upstream verification does not establish paid-service latency or future CLI compatibility. The benchmark includes PostgreSQL and in-process API work with full logging to a local file; it excludes real provider latency, client network transport and deployment log collectors. Reported percentiles are means of per-run statistics, not pooled request percentiles.

A bounded non-lossy log queue can block Tokio emitters when its sink is saturated, and queue capacity bounds record count rather than bytes. A stalled sink can delay normal drain indefinitely; sink errors are reported. Token rotation cannot be atomic with a remote OAuth server if the response or subsequent database commit is lost. These limits are preserved and must remain explicit in delivery documentation.

Deployment assets are unchanged; Helm/container checks were not repeated for this runtime-focused change.
