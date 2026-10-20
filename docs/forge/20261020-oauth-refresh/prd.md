# Background OAuth refresh

Keep configured Codex and Claude OAuth credentials refreshed while the proxy is running, including periods with no inference traffic. The deployment is personal and may have multiple replicas sharing PostgreSQL.

## Required behavior

- Check credentials at startup and periodically, refreshing before access-token expiry and preventing indefinite inactivity when expiry metadata is unavailable.
- Use the providers' OAuth refresh flow without generating inference requests or consuming model quota.
- Coordinate maintenance and inference through existing PostgreSQL credential locks. Reload after locking and persist rotated tokens through the same transaction connection.
- A failed or missing credential must not stop the server or other providers. Log provider identity and a safe error classification, never tokens or upstream bodies. Retry on subsequent maintenance passes.
- Stop scheduling on shutdown and let a refresh that may rotate credentials finish and persist before exit.
- Preserve ordinary inference behavior, provider isolation, and the existing official-binary container deployment.

## Scope and verification

Use existing runtime dependencies and provider abstractions. No dashboard, login UI, configurable scheduler framework, or billable keepalive is required. Prefer extending provider integration fixtures and exercise idle refresh, fresh-token skipping, concurrent replicas, rotation persistence, failure/retry, and shutdown behavior with a disposable database and fake OAuth services.

Automatic refresh cannot restore already revoked or expired refresh tokens, and cannot run while the proxy is stopped. Exact provider inactivity limits are not assumed.
