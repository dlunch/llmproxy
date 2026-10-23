# Account management API delivery

## Result

The API now supports signup-code issuance, email/password signup and session login, multiple inference tokens, per-token metrics, provider registration/configuration, durable initial OAuth login for both backends, and atomic account routing replacement. The requested static dashboard is the next delivery phase and separate commit.

## Implementation

- One SQL script issues a single-use signup code; only its hash is stored. Argon2 password work is bounded off the async executor. Management uses a 24-hour HttpOnly session cookie, custom mutation header, ownership-scoped SQL, and no-store responses.
- Provider instances and routes live in PostgreSQL. Two shared backends accept the selected instance ID; all replicas observe new configuration without restart. Both least-used and priority policies preserve quota exclusion.
- OAuth attempts persist across replicas. Claimed exchanges survive HTTP cancellation, serialize credential updates, clear terminal secrets, and use generations to separate quota/callbacks after relogin.
- Multiple client tokens have separate Prometheus labels and durable usage/outcome summaries. Rejections before durable admission have a separate counter.
- Self-service CIDR management was removed during review because a session does not prove network ownership. Existing operator-provisioned IP authentication is unchanged.

## Verification and review

The final full suite passed 28 unique tests, including PostgreSQL integration, actual HTTP routing/authentication, OAuth mock servers, and the installed official Claude CLI against a local mock Messages API. A child-process logging test reruns one of those cases; it is not counted twice. Formatting, locked all-target Clippy with warnings denied, chart lint/render checks, and independent QA results are recorded in `qa-report.md`.

Architecture review resolved relogin generation fencing, PostgreSQL lock-snapshot mixing, and expiry/replacement races. Implementation review removed unsafe extra CIDR scope and bounded background refresh to four jobs, preserving started work while skipping queued jobs on shutdown.

## Persistence and latency

Routing performs two parallel reads instead of three, with no request-history aggregation on inference. Fresh credential reads remain one statement; the locking path adds one generation read. Quota writes remain batched. Account/provider totals still update atomically with finalization. Session cleanup uses the existing periodic loop and skips active OAuth locks.

The release matrix covers 24 scenarios twice per version. Timing was noisy: averaged per-run p50 decreased in 14 cases and increased in 10, spanning -24.5% to +97.3%. One incomplete baseline run returned 503 and was retried successfully. These data do not establish a general speedup or regression; see `performance.md` and all successful samples in `latency-samples.csv`.

## Limits

No real subscription account or Kubernetes deployment was exercised. The official Claude login protocol may change. Abrupt process loss after upstream authorization-code consumption but before DB commit requires a fresh login. Operators upgrading an old database must follow the documented email/password/provider-kind cutover; obsolete static provider configuration is rejected.
