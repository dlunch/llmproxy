# Background OAuth refresh architecture

## Design and boundaries

The application runs one credential-maintenance loop for the configured provider registry. Each pass checks all providers concurrently, without inference requests, quota charges or Claude CLI invocations. Provider adapters own OAuth protocol details; PostgreSQL remains the shared source of credential state and synchronization across replicas. Existing Tokio, reqwest, chrono and SQLx dependencies suffice.

The existing `Provider` trait gains `async fn refresh_credentials(&self) -> Result<(), ProxyError>` with a default no-op for providers without refreshable credentials. A shared due-check function in the existing providers module owns these timing rules for both adapters. A small provider-maintenance function receives configured `(provider_id, Arc<dyn Provider>)` entries and a cancellation token. `main` starts it after provider construction and stops it during application shutdown. The gateway's request-routing behavior remains independent of maintenance.

## Schedule and lifecycle

A Tokio interval ticks immediately at startup and every 60 seconds thereafter, with missed ticks skipped. Each pass awaits all provider refresh calls concurrently; each result is logged independently. A slow or failing provider does not prevent another provider from refreshing. Passes do not overlap. No new scheduling configuration is introduced.

Cancellation stops scheduling new passes. Once a pass starts, the service awaits its OAuth calls and database commits; it must not abort a refresh after the upstream may have rotated its token. OAuth HTTP calls have a 30-second timeout. Maintenance uses nonblocking row acquisition so a Claude inference holding its row cannot stall shutdown. On shutdown, signal the maintenance cancellation token, drain HTTP requests, await maintenance, and preserve the existing gateway/provider worker drain. Server errors also stop and join maintenance before returning.

## Eligibility and expiry

Only configured provider instances whose owning account is enabled are checked. Missing credential rows and credentials without a refresh token are ordinary skips, including Claude credential documents without `claudeAiOauth`. Disabled accounts require no new activation flag or schema column.

Known access-token expiry is due at or before `Utc::now() + 5 minutes`; additionally, refresh at least once per 24 hours since the last successful token-state update, even when access expiry is farther away. Codex reads its typed `expires_at`; Claude reads `claudeAiOauth.expiresAt` in epoch milliseconds. Unknown expiry uses a bounded hourly refresh interval based on the last credential token-state update. Initial credential import starts that interval from the row's existing `updated_at`; missing expiry therefore never disables maintenance indefinitely.

Reuse `updated_at` rather than adding a parallel scheduling timestamp. Codex already updates this column only when saving authentication. Claude's ordinary inference save must preserve it when OAuth access token, refresh token and expiry are unchanged. Update it when those token fields change, including CLI-driven rotation. A successful direct Claude refresh sets a new expiry from the token response and advances the timestamp. Profile changes and identical credentials do not postpone unknown-expiry refresh. The timestamp enters the credential read model because it now drives runtime behavior.

## Transaction and provider interfaces

Existing blocking `lock_codex_credentials` / `lock_claude_credentials` remain the inference paths. Add corresponding maintenance `try_lock_*_credentials` methods returning `Result<Option<(Transaction<'static, Postgres>, Credentials)>, ProxyError>`. They join provider ownership to enabled accounts and select the credential row with `FOR UPDATE OF credentials SKIP LOCKED`. Both blocking and nonblocking readers include `updated_at`.

Every maintenance attempt follows this sequence on one transaction/connection:

1. Acquire the credential row and load its current state; skip missing, disabled or already locked rows.
2. Check refresh-token eligibility and expiry using the state just read under the lock.
3. If not due, commit and release the row immediately.
4. Perform the provider's refresh-token exchange while retaining the lock.
5. Persist returned access token, rotated refresh token and expiry through the existing save method, then commit before reporting success.

A competing replica subsequently reads the new expiry/timestamp and skips a duplicate refresh. A maintenance worker never uses a pre-lock credential snapshot. Inference retains the same row-lock discipline, including Claude's lock through CLI execution and credential readback.

Codex should share its due decision and locked refresh/save operation between request-triggered and maintenance refresh, replacing the current 60-second request-only expiry window with the same five-minute rule, 24-hour maximum refresh age and hourly unknown-expiry fallback. Keep its tracked refresh task so request cancellation cannot discard a rotated token. Claude keeps inference through the official CLI; direct maintenance refresh updates the same JSON credential document under the same lock, and CLI-driven rotations use the corrected timestamp persistence.

## Claude OAuth boundary

Add a small `ClaudeAuth` client alongside the Claude provider, injected from `main` using the existing HTTP client and fixed production token endpoint. Tests inject a local endpoint, as existing Codex tests already do.

The official Claude Code 2.1.304 protocol is `POST https://platform.claude.com/v1/oauth/token` with JSON `grant_type=refresh_token`, the stored refresh token, stored `clientId` or the official default client ID, and stored scopes joined by spaces (official defaults when absent). The response supplies `access_token` and `expires_in`, optionally `refresh_token`, `scope` and `refresh_token_expires_in`. Validate this external response; preserve an omitted refresh token, unrelated OAuth fields, other credential-document fields and database profile metadata. Convert relative expiries to epoch milliseconds. Do not invoke auth status or send a model request: status does not refresh credentials, and inference would be billable.

## Errors and observability

Provider/network/response/database failures affect only that provider's attempt. Roll back uncommitted work, retain the previously persisted credentials and retry on the next scheduled pass. Use structured logs containing provider ID and the existing coarse `error_kind`; never log token bodies, request/response payloads or raw network errors. A row-lock skip is normal and needs no warning. Successful refresh may emit a token-free provider event; avoid logging unchanged checks every minute.

Upstream refresh-token revocation and hard login expiry still require reauthentication. Maintenance does not promise to extend an absolute login lifetime. An upstream rotation followed by a lost response or failed database commit cannot be made atomic across systems; surface the redacted error and preserve graceful draining rather than claiming guaranteed recovery.

## Implementation blocks and verification

| Order | Responsibility | Interfaces and dependencies | Verification |
|---|---|---|---|
| 1 | Credential transaction/freshness persistence (core) | Existing store readers/savers plus nonblocking maintenance readers; `updated_at` in credential models | PostgreSQL integration: disabled/missing/locked rows skip; unchanged Claude inference saves preserve timestamp; real token changes advance it |
| 2 | Provider refresh operations (core) | `Provider::refresh_credentials`; existing `CodexAuth`; new narrow `ClaudeAuth`; store block | Extend existing provider fixtures: idle due refresh, fresh skip, unknown-expiry bounded refresh, malformed/failed response leaves state, preserved metadata, rotated tokens persisted, replica and inference concurrency |
| 3 | Application maintenance lifecycle (core) | Configured registry, cancellation token, Tokio interval and concurrent futures; provider block | Focused lifecycle test with observable provider calls: immediate pass, later pass, isolated failure, no overlapping passes, stop scheduling and drain in-flight refresh |

Implement storage/interface changes first. Codex and Claude protocol work can proceed in parallel once their signatures agree. Wire scheduling after provider hooks are verified. Reuse current provider concurrency/cancellation integration tests rather than duplicating their coverage. Test local OAuth endpoints only; no real credential refresh or billable provider inference is required for this feature's verification. Keep scheduling tests independent of live network time; use Tokio's test clock if needed, without adding production knobs.

## Project classification and assumptions

Backend service; no additional frontend/mobile guideline applies. Fixed defaults are a 60-second check, five-minute expiry lead, 24-hour maximum refresh age for known expiry and one-hour fallback for unknown access expiry. Credentials imported without expiry begin the fallback interval at their stored timestamp. No new configuration, database schema, daemon process or dependency is required. Existing external credential JSON remains document-shaped; refresh scheduling state uses the existing typed timestamp.

Protocol evidence: provider research inspected the unmodified official Claude Code 2.1.304 executable. [Claude authentication documentation](https://code.claude.com/docs/en/authentication#renew-an-expiring-login) documents hard-login renewal limits. Codex refresh reuses the repository's existing implementation derived from official Codex source.
