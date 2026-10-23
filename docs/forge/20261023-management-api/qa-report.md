# Management API QA report

## Result

- **Verdict: PASS** for the management API implementation and the documented validation scope.
- **Final automated suite:** 28 distinct tests passed, 0 failed, 0 ignored. The manual latency benchmark was deliberately filtered out of this run and executed separately by the coordinator.
- **Other checks:** formatting, locked all-target Clippy with warnings denied, Helm rendering assertions, Helm lint, and tracked-diff whitespace checks passed.
- **Unresolved findings:** Critical 0, High 0, Medium 0, Low 0. The resolved implementation findings are recorded in `implementation-review.md`.

The first full-suite attempt found compiler error E0373 in the newly bounded maintenance closure before any tests ran. The coordinator changed the closure to capture provider/stop references before an `async move` block. The complete rerun and Clippy passed after that fix.

## 1. Requirements and coverage

| Requirement or invariant | Evidence | Result |
| --- | --- | --- |
| Single-use signup code; duplicate-email rollback; concurrent redemption | `signup_codes_password_sessions_and_account_isolation` uses the repository issuance SQL and real account-creation HTTP requests | Pass |
| Email normalization, password verification, cookie flags, logout, session expiry, disabled accounts | Same management test; stored Argon2 PHC is checked; wrong passwords, stale sessions, and disabled login fail | Pass |
| Session-only management; required mutation header; tokens returned once | Same test rejects missing header/session and inference bearer credentials, verifies token read responses omit plaintext, and checks `no-store` | Pass |
| Account ownership for tokens, provider metadata, provider login and route writes | Both management API tests plus `browser_logins_are_owned_persistent_single_use_and_refreshable` exercise foreign IDs and verify 404/rollback | Pass |
| Multiple tokens and distinct durable/Prometheus attribution | `provider_configuration_atomic_routing_and_per_token_history` performs actual Chat/Responses inference, upstream failure, route/quota rejection, and revoked-token rejection; checks persisted rows and both metric registries | Pass |
| Runtime configuration across replicas, priority/least-used routing, exhausted quota, provider disablement | Two gateways are constructed before registration, then management HTTP changes affect both; connected gateway tests preserve lifecycle and quota behavior | Pass |
| Routing replacement is atomic and foreign/duplicate mappings are rejected | Management API test verifies the prior strategy/routes survive each failed replacement; production snapshot is one SQL statement | Pass |
| Codex device login, polling schedule, completed replay and expiry | Management OAuth test uses two managers/pools and a local fake identity service; connected Codex tests verify exchange/refresh and inference headers/events | Pass |
| Claude PKCE state, secret handling, failed/malformed exchange, credential preservation and relogin generation | Management OAuth test checks authorization challenge/state, persisted credentials, cleared transient secrets, sanitized errors, and preserved previous credentials | Pass |
| Cancellation, active-exchange expiry, cleanup/replacement ordering and graceful drain | `dropped_completion_and_expiry_do_not_discard_an_exchange` aborts its HTTP task during a blocked exchange, attempts cleanup/replacement, and checks the saved credential/generation | Pass |
| Refresh coordination and generation-fenced old requests | Existing provider tests coordinate refresh across replicas; a real management relogin makes an old Claude child fail without receiving the new token; gateway/store test rejects late old-generation quota | Pass |
| Initial management login supplies usable official Claude CLI credentials | `actual_cli_roundtrips_parallel_calls_against_a_local_messages_api` creates credentials through management HTTP, runs the actual executable, refreshes them against a local fake API, and round-trips tools/results | Pass |
| DB-driven maintenance discovers registration, isolates failures, bounds work and stops queued shutdown work | Extended maintenance test discovers newly registered providers, excludes disabled rows, blocks eight jobs, observes four starts, then verifies cancellation starts no queued jobs | Pass |
| Existing inference, token/IP authentication, logging, accounting, cancellation and tool behavior | All existing API, provider, gateway and logging tests run, including ignored PostgreSQL/local-CLI cases | Pass |
| Host-only configuration and deployment examples | Config test rejects old static provider entries; Helm checks default/custom host config, secret requirements, replicas, probes and graceful termination settings | Pass |

No mandatory behavior is left without meaningful automated coverage or a directly inspected invariant. Per-token admission/rejection assertions now use actual inference through the HTTP/gateway/store path rather than inserted synthetic request-history rows.

## 2. Test quality audit

Assertions check externally observable statuses, response contents, persisted credentials/request records, metric attribution, and process lifecycle. Fake servers control external OAuth/provider behavior while real HTTP handlers, SQL transactions, routing, persistence, and subprocess integration remain active. Each database fixture creates an isolated schema; tests do not rely on execution order. The official executable fixture supplements protocol doubles.

Concurrency tests use explicit notifications/semaphores for exchanges and worker drains. Small deadline waits exercise actual expiry/cleanup behavior. No empty error catches, always-true assertions, production APIs introduced solely for tests, or tests replacing the critical implementation with a mock were found. The logging test intentionally starts a child test process; its second printed success is not counted as a second distinct test.

## 3. Executed checks

With `TEST_DATABASE_URL` set to the disposable PostgreSQL database and `CLAUDE_TEST_EXECUTABLE=/usr/bin/claude`:

```sh
cargo test --locked -- --include-ignored --skip proxy_latency_benchmark
cargo fmt -- --check
cargo clippy --locked --all-targets -- -D warnings
python3 tests/helm_chart.py
helm lint charts/llmproxy --set database.existingSecret=qa-postgres
git diff --check
```

Final Rust counts: logging 2; API 8; Claude provider 8; Codex provider 2; configuration 1; maintenance 1; gateway/store 2; management API 2; management OAuth 2. All passed. Helm lint emitted only its informational icon recommendation. Full test, Clippy and formatting logs were saved as `/tmp/llmproxy-management-full-tests.txt`, `/tmp/llmproxy-management-clippy.txt`, and `/tmp/llmproxy-management-fmt.txt`; the initial compiler failure is retained separately.

## 4. Performance evidence and limits

The coordinator ran the unchanged release API benchmark separately from builds/tests, with two successful baseline runs and two successful changed-version runs. `latency-samples.csv` contains 96 records for 24 conditions, covering both APIs, streaming/collected output, concurrency 1/4, large output and 100,000 historical requests. An independent CSV calculation confirmed that averaged run p50 improved in 14 conditions and regressed in 10, ranging from -24.5% to +97.3%.

The host produced large repeated-run differences for the same binary. Therefore these samples do **not** establish an overall latency or throughput improvement or regression. One additional baseline run failed with HTTP 503 and was excluded; its error body was not preserved, so its cause is unproven. A proposed gateway permit-release explanation was not treated as an established defect and caused no unrelated production change.

Structural inspection confirms routing reads decrease from three to two, the ordinary fresh-credential read remains one query, and the extra generation read applies only to an existing credential-lock/refresh path. Those query-count facts are not a substitute for a stable performance measurement.

## 5. Validation boundaries

| Area | Boundaries of this result |
| --- | --- |
| Live subscriptions | OAuth/provider responses are local fixtures; no real Codex or Claude account login, entitlement, upstream latency or live renewal was tested. The actual Claude executable was used only against the fake local API. |
| Deployment | Helm manifests were rendered and linted; no Kubernetes rollout or production database cutover was performed. Existing-account/provider cutover prerequisites are now documented in README. |
| Performance | Local PostgreSQL, API handling and logging are included; real upstream transport, client network drain and deployed log/metrics collectors are excluded. A controlled host is needed to resolve the observed performance variability. |
| Crash boundary | A hard process kill between an external consuming exchange and database commit cannot be made atomic; the documented recovery is a new login. Graceful cancellation/shutdown persistence is tested. |
| Dashboard | The separately requested static dashboard is outside this API commit and requires its own browser QA. |

The API is ready for its separate commit within these stated limits. No outstanding code correction is required by this QA pass.
