# QA report

Final result: PASS. Independent full Rust suite: 19 passed, 0 failed, 0 ignored. No unresolved Critical, High, Medium or Low findings.

## Coverage against the architecture

| Required behavior | Evidence | Result |
|---|---|---|
| Collect both explicit windows | Extended `subprocess_requires_successful_result_and_exit` | PASS |
| Sparse updates retain independent values and timestamps | Same fixture sends a later five-hour observation and invalid seven-day update; stored seven-day observation remains unchanged | PASS |
| Missing, malformed and unknown fields do not invent quota | Same fixture covers absent windows, malformed utilization, invalid reset and unrelated/top-level fields | PASS |
| Utilization above one means no remaining base capacity | Same fixture returns seven-day utilization 1.2 and checks zero remaining | PASS |
| Preserve measurements across function handoff | Extended `parallel_function_handoff_preserves_calls_and_cumulative_search_usage` | PASS |
| Preserve already observed measurements on cancellation/drop | Extended `cancellation_and_dropped_stream_terminate_the_subprocess` | PASS |
| One-connection pool completes without nested acquisition | All Claude fixtures use `max_connections(1)`; completion and persistence passed | PASS |
| Quota failure retains credential rotation and fails completion | Extended `concurrent_calls_reload_rotated_credentials_and_persistence_errors_fail_requests` injects a PostgreSQL quota trigger failure | PASS |
| Actual official CLI emits the supported format | `actual_cli_roundtrips_parallel_calls_against_a_local_messages_api` with unmodified 2.1.304 and local dummy OAuth; quota headers produce stored remaining fractions 0.75/0.6 | PASS |
| Existing API, routing, OAuth refresh and cancellation behavior | Complete Rust suite | PASS |

## Test quality

Tests exercise the actual provider subprocess path and PostgreSQL state rather than duplicating parser implementation. The official CLI test independently verifies header-to-event-to-database behavior. Database trigger failure proves auth rotation survives a separate quota write failure. Fixture pools have one connection, exposing accidental nested transaction acquisition. Existing tests were extended; no test-only production API or extra dependency was added.

No vacuous assertions, swallowed unexpected errors or excessive mocking were found. No required coverage gap remains for this feature.

## Executed verification

- `cargo test --all-targets -- --include-ignored` with local `TEST_DATABASE_URL` and official `CLAUDE_TEST_EXECUTABLE`: 19 passed. Breakdown: API 7, Claude 7, Codex 2, configuration 1, maintenance 1, gateway 1.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy --all-targets -- -D warnings`: passed.
- `git diff --check`: passed.

## Limits

Tests use dummy credentials and local fake upstream responses; no real Anthropic quota or billable inference was consumed. The official CLI test verifies version 2.1.304, not arbitrary future event formats. The existing routing policy conservatively excludes fresh exhausted base windows even if paid overage is available; overage-aware selection is outside this accepted feature scope. No throughput or load claim is made beyond the exercised concurrency fixtures. Deployment assets were unchanged and were not revalidated for this provider-only change.
