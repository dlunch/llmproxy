# Background OAuth refresh

## Purpose and decisions

Keep Codex and Claude subscription credentials refreshed while the proxy is running without inference traffic. Use one startup/one-minute maintenance loop over configured providers and existing PostgreSQL credential locks to coordinate with inference and other replicas.

Refresh within five minutes of known expiry or after 24 hours without a token update; use an hourly fallback when expiry is unavailable. Preserve Claude's credential timestamp during ordinary saves unless OAuth token state changes. Call OAuth endpoints directly, preserve rotated credentials, and drain in-flight maintenance during shutdown.

The implementation reuses existing dependencies and tables. Each replica adds at most one credential-row lookup per configured provider per minute when passes complete within that interval; only due tokens cause an OAuth request and credential write. Locked rows are skipped until a later pass.

## Artifacts

- [Requirements](../forge/20261020-oauth-refresh/prd.md)
- [Architecture](../forge/20261020-oauth-refresh/architecture.md)
- [Final report](../forge/20261020-oauth-refresh/final-report.md)

## Operational limits

Revoked refresh tokens or absolute login expiry require reauthentication. No maintenance runs while the proxy is stopped. OAuth rotation and the database commit cannot form a single transaction across services.
