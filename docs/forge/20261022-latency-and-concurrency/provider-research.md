# Claude concurrent inference and host-owned OAuth

## Finding

The official Claude Code 2.1.307 executable supports host-owned refresh through its existing stream-JSON control protocol. Use that protocol with access-token-only injection; a fixed environment token alone would regress recovery when a token expires during a multi-call inference.

Official documentation describes `CLAUDE_CODE_OAUTH_TOKEN` as an OAuth access token that takes precedence over stored credentials and normally remains fixed for the session: https://code.claude.com/docs/en/env-vars . Subscription authentication and locally configured MCP servers remain supported: https://code.claude.com/docs/en/authentication . The refresh bridge below is verified in the shipped executable, rather than promised by that public environment-variable reference.

## Verified runtime behavior

Inspected the installed unmodified executable reporting version 2.1.307. Its OAuth environment-token constructor sets `refreshToken` and `expiresAt` to null, reads optional `CLAUDE_CODE_OAUTH_SCOPES`, `CLAUDE_CODE_SUBSCRIPTION_TYPE`, and `CLAUDE_CODE_RATE_LIMIT_TIER`, and takes precedence over `.credentials.json`. Scopes default to `user:inference` when absent. Do not pass a refresh token to the child.

The host callback is enabled by `CLAUDE_CODE_SDK_HAS_OAUTH_REFRESH=1` with `CLAUDE_CODE_ENTRYPOINT=local-agent`. The executable's allowlist also contains desktop and VS Code entrypoints; use the local-agent identity for this host. Do not set the separate desktop launcher / provider-managed-by-host variables.

After a model API 401 without a local refresh token, the CLI emits:

```json
{"type":"control_request","request_id":"request-id","request":{"subtype":"oauth_token_refresh"}}
```

The host answers:

```json
{"type":"control_response","response":{"subtype":"success","request_id":"request-id","response":{"accessToken":"replacement-access-token"}}}
```

A null access token declines refresh. The CLI accepts a nonempty replacement different from the rejected token, updates its environment token, clears its credential caches, and retries through its existing recovery path. The callback has a 30-second timeout. In ordinary environment-token mode the CLI deliberately does not fall back to a stored credential after a 401.

A standalone local probe used a fresh temporary HOME/config directory, dummy tokens, and a fake loopback Messages API. The API returned 401 for the first call. The unmodified CLI emitted the refresh control request; after the replacement response the API observed `Bearer dummy-old` followed by `Bearer dummy-new`. The CLI returned text, a successful result, a five-hour quota observation, and exit code 0. No credential file or account metadata file was supplied. This proves protocol/auth-mode behavior, not live service token validity.

## Proposed ownership and lifecycle

- Read PostgreSQL credentials without an exclusive lock on the fresh-token path. Before spawning a CLI, perform existing proactive refresh when due.
- Both request preparation and periodic maintenance call one coordinated host refresh path. Acquire the credential row lock, re-read, and check freshness again. Only the actual refresh owner exchanges and commits the rotated token. Track started refresh work independently of the request so cancellation cannot discard the new refresh token.
- Give each child only the current access token plus stored scope/subscription/rate-tier metadata. Keep the existing private working/config directory and conversation/tool protocol. A `.claude.json` snapshot may supply imported account identity/onboarding metadata; it is not a concurrent credential writer.
- Handle `oauth_token_refresh` within the existing control-request pump. Remember the access token last handed to this child. Under the row lock, if PostgreSQL already has a different token, return that token without another exchange; otherwise force refresh even if the stored expiry appears fresh. Commit before replying and update the remembered token after a successful reply.
- Keep the row lock and database connection only through refresh persistence, never through CLI inference. Remove child credential hydration, post-exit credential-file parsing/persistence, and obsolete tests/helpers. Keep quota persistence independent and preserve subprocess cancellation, reaping, stream backpressure, terminal errors, and shutdown draining.
- If the child stops or its callback times out, an exchange already started still finishes and persists. No inference replay is added at the proxy layer: the CLI handles the rejected API attempt in place, preserving existing tool/conversation behavior.

## Required regression evidence

Extend the existing fixtures rather than duplicate them:

1. Two provider instances sharing one credential row reach independently blocked fake CLI/upstream calls concurrently; fresh credentials cause no auth UPDATE and no lifetime row lock.
2. Concurrent initial-expiry refresh and maintenance/inference races exchange once and both children receive the committed token, including separate PgPools with a single connection each.
3. Concurrent 401 callbacks for the same rejected token exchange once; a token already changed by another replica is reused. Repeat a 401 after one successful replacement to verify per-child token tracking.
4. Drop/cancel the inference while a refresh exchange is blocked; release the exchange and verify rotated credentials commit and shutdown drains. Keep existing dropped-stream/process-reaping and known-usage/quota persistence checks.
5. Extend the official-CLI/local-API test to assert the real 401 callback and replacement Authorization header, alongside its existing tool handoff, resumed conversation, and five-hour/weekly quota assertions.
6. Failures from refresh and persistence remain errors; a failed refresh must not start a child with known expired credentials. A callback timeout may fail the active request while the committed credential benefits subsequent requests.

The bridge is an internal CLI protocol, so the official-binary regression test is essential when updating the bundled CLI. No live subscription calls or real credentials were used in this research.

## Implementation timing contract

Credential reads, row-lock acquisition, and refresh HTTP through credential commit use scoped timers so errors and dropped waiting futures are observed. Request preparation and CLI callbacks use `credential_read`, `credential_lock`, and `credential_refresh`. Periodic maintenance uses separate `maintenance_credential_lock` and `maintenance_credential_refresh` series; its frequent skipped-lock checks do not dilute inference latency distributions. Only an actual token exchange contributes a refresh observation. A cancelled caller's already-started refresh remains tracked until commit and contributes its full duration.

The integrated provider tests also compare fresh-path lock counts and count refresh observations across success, rejection/persistence failure, and cancellation. These assertions complement the held-row-lock and delayed-refresh concurrency checks.
