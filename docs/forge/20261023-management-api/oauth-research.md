# Management API OAuth research

Date: 2026-10-23

## Recommendation

Expose provider-owned login sessions through authenticated management APIs. Use the existing direct Codex device protocol and the official Claude Code manual PKCE flow. Store pending state in PostgreSQL, so the next API request may reach another replica or a replacement Pod. No browser callback listener, CLI login subprocess, sticky session, or shared filesystem is needed.

The user's final account decision requires a database-backed signup code issued by a script; this supersedes the earlier public-registration answer. Signup uses email/password plus that code. Management uses a separate finite-lived HttpOnly session cookie; inference API tokens never authorize management. The OAuth endpoints use the authenticated management session and enforce the same account ownership boundary. All mutations also require X-LLMProxy-Management: 1.

This is a protocol-level implementation of the official clients, not a documented third-party OAuth integration contract. The existing inference implementation already depends on their subscription protocols. Keep fixed production endpoints and fixture coverage; do not let account settings select arbitrary OAuth URLs, executable paths, client IDs, or redirects.

## Evidence and source boundaries

- [OpenAI authentication documentation](https://learn.chatgpt.com/docs/auth#login-on-headless-devices) describes device authentication, enabling it in account/workspace settings, and entering a one-time code in the browser.
- Existing `src/providers/codex.rs` follows official Codex revision `091008729d5572712ac08964a9602eb71cf31428`, `codex-rs/login/src`. Its requests, token parsing, and fixtures are the reuse point. The direct GitHub page fetch was unavailable during this pass; no claim of revalidating the current upstream revision is made.
- [Claude authentication documentation](https://code.claude.com/docs/en/authentication#log-in-to-claude-code) describes manual URL/code entry when a browser cannot reach a local callback, including containers.
- [Claude CLI reference](https://code.claude.com/docs/en/cli-reference) documents `claude auth login` but no HTTP or structured JSON login interface.
- Read-only inspection of the installed official Claude Code 2.1.307 executable confirmed the protocol below. SHA-256: `6021eb72d20d0682b720bdc63acd2a5cd2270895d1f10d9ca2a88cbfa7b9dd0e`. The version/help commands and bundled JavaScript were inspected; no credentials were read and no live login or model request was made.

## Codex device authentication

Reuse `CodexAuth` and its validated token response conversion. Production issuer is `https://auth.openai.com`; client ID is `app_EMoamEEZ73f0CkXaXp7hrann`.

1. POST `/api/accounts/deviceauth/usercode` with JSON `client_id`.
2. Persist returned `device_auth_id`, `user_code`, polling interval, next allowed poll time, and a 15-minute expiry. Return the session ID, `https://auth.openai.com/codex/device`, user code, expiry and polling interval to the owner.
3. An authenticated completion/poll request POSTs `/api/accounts/deviceauth/token` with the stored device ID and user code. Existing implementation treats 403/404 as pending.
4. On authorization, the response supplies `authorization_code` and `code_verifier`. Exchange those using form encoding at `/oauth/token`, with `grant_type=authorization_code`, the fixed client ID, and `redirect_uri=https://auth.openai.com/deviceauth/callback`.
5. Save the resulting access token, refresh token, ChatGPT account ID and expiry in `provider_credentials`. Return status, never the upstream tokens.

Replace process-local `Instant` session fields with persisted UTC timestamps and a typed polling interval. Do not hold a database row lock while waiting for the next poll time. A request arriving too early can immediately return pending with the remaining interval. Coordinate the poll deadline and completion claim across replicas before making upstream requests.

## Claude manual PKCE authentication

The bundled functions for URL construction, exchange, manual input and token formatting establish these values:

| Value | Official Claude Code 2.1.307 behavior |
| --- | --- |
| Authorize URL | `https://claude.com/cai/oauth/authorize` |
| Manual redirect | `https://platform.claude.com/oauth/code/callback` |
| Token URL | `https://platform.claude.com/v1/oauth/token` |
| Client ID | `9d1c250a-e61b-44d9-88ed-5944d1962f5e` |
| Code challenge | Base64url without padding of SHA-256 of the verifier |
| Manual input | `authorization_code#state` |
| Token exchange timeout | 30 seconds |

Generate a fresh cryptographically random verifier and state for each login. Persist both before returning the authorization URL. A verifier must satisfy PKCE's 43–128-character requirement; two concatenated UUID v4 simple strings use the existing dependency and supply 64 allowed characters with ample entropy. Do not return the verifier.

The authorization URL has these query parameters:

- `code=true`
- `client_id` and `response_type=code`
- fixed `redirect_uri`
- `scope`
- `code_challenge` and `code_challenge_method=S256`
- `state`

The official default initial-login scope set is `org:create_api_key user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload user:plugins`. The existing proxy's refresh scope defaults are the same except for `org:create_api_key`. The CLI itself also supports inference-only/custom scope flows, but this pass did not establish a narrower initial scope combination that preserves renewable subscription credentials. Avoid inventing a narrower grant while claiming exact CLI parity.

The user opens the returned URL in a local browser and submits the full displayed `code#state` to the authenticated completion endpoint. Validate nonempty code and state equality against the stored session before any consuming operation. Do not accept an arbitrary callback URL or use a caller-selected redirect.

Exchange JSON contains:

```json
{
  "grant_type": "authorization_code",
  "code": "<code portion>",
  "redirect_uri": "https://platform.claude.com/oauth/code/callback",
  "client_id": "9d1c250a-e61b-44d9-88ed-5944d1962f5e",
  "code_verifier": "<stored verifier>",
  "state": "<stored state>"
}
```

The token response contains `access_token`, `refresh_token`, `expires_in`, `scope`, optionally `refresh_token_expires_in`, and optional `account` / `organization` / `workspace` objects. Persist the same existing `claudeAiOauth` shape used by inference and maintenance: accessToken, refreshToken, expiresAt in milliseconds, scope array, and refreshTokenExpiresAt when the server supplies it. Reject missing/empty renewable credentials and nonpositive or unrepresentable expiry values at the upstream response boundary. Scope must include `user:inference` for the requested provider use.

The token account fields may populate `oauth_account`: `account.uuid` becomes `accountUuid`, `account.email_address` becomes `emailAddress`, and `organization.uuid` becomes `organizationUuid`. Other values are optional. `user_id` may remain null; onboarding can be marked complete after successful subscription login.

### Profile metadata is optional

The official CLI optionally GETs `https://api.anthropic.com/api/oauth/profile` with the access token. Its profile function handles failures by returning no profile; token formatting permits null subscriptionType/rateLimitTier. The existing proxy's CLI invocation supplies optional account metadata, scopes and plan hints, and previously verified official-CLI fixtures can run with access-token-only injection.

Therefore, an additional profile request should not gate persisting newly issued tokens. The minimal integration uses account metadata already in the token response and leaves missing profile hints absent. A future account-display requirement could justify fetching a profile after credentials are safely stored; the current management API does not require it.

## Session persistence and concurrency

Keep login state in typed relational columns rather than serializing an enum into JSON. A session needs an opaque UUID, provider reference, expiry, status, and provider-specific fields: Codex device ID/user code/interval/next poll time; Claude state/verifier. Derive account ownership through the provider relationship. A session is never authorized solely by its UUID.

Suggested API shape, with final naming left to the architecture:

- Start a provider login: returns session ID, expiry, browser URL, and a user code only for Codex.
- Read session status: returns only safe status and scheduling information.
- Complete/poll: Codex polls the device grant; Claude accepts the pasted authorization response.

Only one active login attempt should be able to replace credentials for a provider. Replacing an old pending session must make its completion ineligible; serialize credential replacement with concurrent login and refresh paths. Completed reads can be idempotent and reveal only status.

Claim an authorization-code exchange durably before submitting it upstream. An ambiguous network failure can mean a code was consumed, so do not blindly retry that exchange. Persist credentials and completed status together, and clear verifier/device secrets when no longer needed. Once token exchange starts, retain the work in a tracked task so dropping the HTTP caller does not discard returned refresh credentials; shutdown must drain these tasks.

Pending sessions can survive restarts. No local design can make a non-idempotent external token exchange atomic with PostgreSQL across an abrupt process kill. If a crash occurs after exchange but before persistence, the session must report failure/expiry and require a new login; do not claim crash-proof completion. A bounded expiry also prevents a stranded exchanging state from remaining indefinitely usable.

Reuse existing account/provider row ownership checks and refresh row-lock ordering. Avoid holding broad account locks or introducing new queries on the inference path merely for login coordination.

## Required integration coverage

Extend existing provider fixtures and add management HTTP integration coverage for observable behavior:

1. Account A cannot start/read/complete account B's provider login; anonymous, invalid/expired management sessions and inference API tokens fail before upstream requests.
2. Codex session created through one service instance is polled through another instance with another pool; early polls cause no upstream request, pending and expiry behave correctly, and successful completion persists credentials.
3. Claude URL has the exact fixed endpoints/query values and a verifier-derived S256 challenge; the API never reveals the verifier. Matching `code#state` exchanges the expected JSON and persists valid credentials.
4. Wrong/missing state, expired session, wrong provider type and repeated consuming completion never exchange a code.
5. Concurrent completion calls exchange at most once and cannot overwrite newer successful login credentials. Old-session replacement is covered by the same test scenario.
6. Drop the HTTP response while a fake exchange is blocked, release it, and verify credentials persist; shutdown drains the work.
7. Malformed/rejected upstream token responses do not replace known-good credentials or expose token/code values in API errors or captured logs.
8. Use the real official Claude CLI against a fake local Messages endpoint with credentials produced by the login fixture, including missing optional profile metadata. This checks runtime integration without claiming real subscription acceptance.

No live subscription authentication, browser callback, real cluster, or third-party authorization agreement was tested. The direct manual Claude protocol is verified against the installed binary and needs revalidation when that binary's authentication flow changes.
