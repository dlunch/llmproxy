# QA report

Final result: PASS. Independent full Rust suite: 19 passed, 0 failed, 0 ignored. No unresolved Critical, High, Medium or Low findings.

## Coverage against the architecture

| Required behavior | Evidence | Result |
|---|---|---|
| Startup/periodic refresh without inference | `maintenance_runs_without_requests_isolates_failures_and_drains_on_shutdown`; inference stub panics if called | PASS |
| Independent provider failures, nonoverlapping passes and shutdown drain | Same virtual-clock lifecycle test with blocked and failing providers | PASS |
| Five-minute lead, 24-hour age and bounded unknown expiry | Extended `codex_streams_and_refreshes_once_with_persisted_quota_and_failure_outcomes`; Claude unknown-expiry scenario | PASS |
| Fresh, disabled, missing and locked credentials skip | Codex and Claude provider integration fixtures with PostgreSQL | PASS |
| Replica coordination and persistence before reuse | Both provider fixtures use separate pools and concurrent refreshes; existing concurrent inference tests remain enabled | PASS |
| Optional rotated token fields and unrelated metadata preserved | `idle_oauth_refresh_preserves_credentials_and_coordinates_replicas` | PASS |
| Rejected, malformed, missing-expiry and oversized OAuth responses | Claude fixture checks retained database state and redacted errors; Codex checks rejection and later retry | PASS |
| Ordinary Claude state saves do not postpone refresh; real token changes do | Extended `subprocess_requires_successful_result_and_exit` | PASS |
| Existing inference cancellation persists rotations and reaps processes | Existing Codex cancellation scenario and Claude cancellation/concurrent-call fixtures | PASS |
| Official Claude CLI compatibility | `actual_cli_roundtrips_parallel_calls_against_a_local_messages_api` with official CLI 2.1.304 | PASS |

## Test quality

Integration tests observe HTTP exchanges, stored credentials, stream outcomes and cross-connection locking. They do not substitute the provider refresh logic. The lifecycle test uses a virtual clock and observable provider calls; its inference method panics, establishing that maintenance cannot silently invoke inference. Fixtures isolate database state. Existing tests were extended rather than duplicating cancellation and inference coverage.

A disabled-account assertion was strengthened to start with due credentials. Test-only Clippy issues were corrected. No vacuous assertions, swallowed unexpected errors or implementation-copy tests remain in the reviewed changes.

## Executed verification

- `cargo test --all-targets -- --include-ignored` with `TEST_DATABASE_URL` pointing to local PostgreSQL and `CLAUDE_TEST_EXECUTABLE` pointing to the official executable: 19 passed. Breakdown: API 7, Claude 7, Codex 2, configuration 1, maintenance 1, gateway 1.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy --all-targets -- -D warnings`: passed after fixing two test-only lint findings.
- `git diff --check`: passed.

The initial sandboxed test attempt could not connect to the local database; the complete suite was rerun successfully with local socket access. No real credential or billable upstream inference was used.

## Limits

Local OAuth endpoints validate request format, response handling, persistence and concurrency, but do not prove live upstream acceptance or long-term account renewal. Upstream revocation and hard login expiry still require reauthentication. An exchange that rotates upstream credentials and then loses its response or fails database commit cannot be atomic across systems. No performance/load claim is made beyond the exercised concurrent fixtures. The changed Rust implementation does not alter deployment manifests or container packaging, so deployment checks were not repeated for this feature.
