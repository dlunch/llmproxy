# Management API implementation review

Scope: the complete uncommitted management API change against `b64ded1`, including new migrations, SQL issuance script, management/store modules, runtime/provider changes, deployment examples, and connected integration tests. The static dashboard belongs to the subsequent commit and is outside this review.

## Findings

### Medium, resolved: bound maintenance concurrency before enumerating self-service providers

`src/providers/mod.rs:35` uses `join_all` over every enabled provider discovered from PostgreSQL. Each due credential refresh retains its transaction and pool connection while its upstream HTTP request runs. `src/main.rs:49` configures sixteen shared connections and a five-second acquisition timeout, while the provider HTTP timeout is thirty seconds. Sixteen simultaneous due refreshes can therefore occupy the entire pool long enough for unrelated inference authentication/routing and management operations to fail acquisition. Dynamic provider registration removes the former small operator-controlled list as a practical bound.

The final implementation uses the existing futures stream with `for_each_concurrent(4)`, retaining most of the shared pool for request work. It stops launching queued jobs after cancellation while draining already-started refreshes through persistence. The existing maintenance integration test now registers eight blocked jobs, observes only four starts, cancels and releases those four, and verifies that queued jobs never start. This finding was based on the actual lock/HTTP/pool lifetimes; no separate load reproduction was run during the coordinator's isolated latency benchmark.

### High, resolved: self-service CIDRs crossed the tenant identity boundary

The initial management draft let any logged-in account add an arbitrary CIDR, although inference authentication chooses the most-specific matching range globally (`src/store.rs:95`). An account could register a victim client's `/32` inside another account's `/24` and redirect headerless inference into the registering account's provider/routes. An exact-network unique constraint did not prevent this, and possession of a management session did not prove address ownership.

The final scope removes the new CIDR management routes, store mutations, schema alterations, and associated CRUD assertions. Existing operator-managed IP authentication remains unchanged. Removal was verified in the final diff and integration test file.

## Reviewed invariants

- Signup-code deletion and account insertion share one transaction; duplicate-email failure preserves the code, and concurrent redemption has at most one winner. Password work precedes transactions and its semaphore permit stays inside the blocking task.
- Management authentication uses a separately hashed, expiring HttpOnly cookie; inference tokens and peer CIDRs do not authorize management. Every mutation requires the custom header, and no CORS authorization is emitted. Resource queries carry account ownership, and returned metadata excludes stored secrets. The coordinator also added `Cache-Control: no-store` at the existing management middleware boundary, with a login-response assertion.
- Routing strategy and model mappings are read in one statement snapshot. Replacement locks the account, verifies provider ownership, and rolls back the whole replacement on failure. Runtime backends are shared by immutable provider kind, with the selected provider ID passed per call.
- OAuth completion claims are durable before consuming an external authorization code. A tracked worker retains session then credential locks across exchange/persistence, with no provider usage-row lock held across network I/O. Expired cleanup skips an active worker; replacement waits for it. Terminal state clears transient secrets, failed replacement preserves working credentials, and ambiguous consuming failures require a new login.
- Refresh-lock paths read credentials first and generation in a subsequent statement. Fresh reads capture both in one nonlocking snapshot. Successful login increments generation transactionally; quota writes reject old generations, routing ignores stale-generation quota, and an old Claude child cannot receive credentials from a later login.
- Durable token summaries scope both token and request ownership. Gateway finalization owns admitted Prometheus metrics; handlers count only pre-admission rejection. Latency naming distinguishes persisted execution time from the full API lifecycle.

## Validation boundaries

This reviewer performed static review and the earlier isolated PostgreSQL locking-snapshot reproduction recorded in `architecture-review.md`. No Cargo tests or load tests were run by this reviewer, to avoid interference with the coordinator's serialized checks and latency measurements. The coordinator reported the management, OAuth, provider, API, gateway, configuration, and maintenance tests passing before the final test extension; those reports are not a substitute for rerunning the affected final test file.

The real official Claude CLI is exercised against a local fake Messages API by the connected fixture. No real subscription login, production deployment, or externally deployed metrics collector is verified. Existing rows with unresolved provider kind require the documented operator cutover; configuration no longer accepts the obsolete static provider list.

Review status: PASS for the implementation after the two findings above were resolved. No additional concrete authentication, ownership, persistence, or routing defect was found. Independent full-suite QA and final performance evidence remain the release gate and are recorded separately in `qa-report.md`.
