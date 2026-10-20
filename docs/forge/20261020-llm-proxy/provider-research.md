# Provider protocol research

Inspected official `openai/codex` source at commit `091008729d5572712ac08964a9602eb71cf31428`. These are observed implementation details, not a guarantee of a stable public subscription API. No real credentials, authentication attempts, or inference calls were used.

## Codex subscription authentication

Sources: [device authentication](https://github.com/openai/codex/blob/091008729d5572712ac08964a9602eb71cf31428/codex-rs/login/src/device_code_auth.rs), [token exchange](https://github.com/openai/codex/blob/091008729d5572712ac08964a9602eb71cf31428/codex-rs/login/src/server.rs), [refresh manager](https://github.com/openai/codex/blob/091008729d5572712ac08964a9602eb71cf31428/codex-rs/login/src/auth/manager.rs).

- Issuer: `https://auth.openai.com`; public client ID: `app_EMoamEEZ73f0CkXaXp7hrann`.
- Begin: `POST /api/accounts/deviceauth/usercode`, JSON `{"client_id":"app_EMoamEEZ73f0CkXaXp7hrann"}`. Response has `device_auth_id`, `user_code` (also accepted as `usercode` upstream), and a string-valued polling `interval` in seconds.
- User opens `https://auth.openai.com/codex/device` and enters the code. No Codex subprocess is involved.
- Poll: `POST /api/accounts/deviceauth/token`, JSON containing `device_auth_id` and `user_code`. HTTP 403 and 404 mean pending; wait the supplied interval, with a 15-minute overall deadline. Other failures terminate. Success supplies `authorization_code`, `code_verifier`, `code_challenge`.
- Exchange: `POST /oauth/token`, form-encoded `grant_type=authorization_code`, `client_id`, `code`, `code_verifier`, `redirect_uri=https://auth.openai.com/deviceauth/callback`. Result includes `access_token`, `refresh_token`, `id_token`. Do not retry a code exchange after an ambiguous response timeout: the one-time code may already have been consumed.
- Refresh: `POST /oauth/token`, JSON `grant_type=refresh_token`, `client_id`, `refresh_token`. Upstream also supplies a refresh-reason field through its OAuth implementation. Persist any rotated access, refresh, and ID tokens atomically. Treat invalid-grant/reused/revoked refresh credentials as requiring login again; do not loop on them.
- Serialize refresh per account across service instances, reload the authoritative credential before refreshing, and preserve existing tokens when an optional replacement token is absent. An inference HTTP 401 may trigger one refresh and replay before any output is delivered.

The browser flow alternatively uses `/oauth/authorize`, PKCE S256, random state, a loopback callback, scope `openid profile email offline_access api.connectors.read api.connectors.invoke`, and parameters `id_token_add_organizations=true`, `codex_cli_simplified_flow=true`, `originator=codex_cli_rs`. Device login avoids owning an HTTP callback and fits the future dashboard.

`id_token` carries account/workspace metadata under the `https://api.openai.com/auth` claim, including `chatgpt_account_id`, `chatgpt_user_id`, and plan metadata. `chatgpt_account_id` is the account-routing header value, not the user ID. JWT payload decoding supplies metadata only; it is not signature verification or authentication of a client. See [token data](https://github.com/openai/codex/blob/091008729d5572712ac08964a9602eb71cf31428/codex-rs/login/src/token_data.rs).

Expose reusable Rust operations for beginning device login, completing its bounded polling/exchange, and refreshing credentials. Return typed challenges/credentials to callers; do not print codes, launch browsers, write CLI auth files, add a management CLI, or add public management/auth endpoints. PostgreSQL rows can be seeded manually initially. Keep credentials out of Debug output, logs, API responses, and Prometheus labels.

## Codex inference

Sources: [provider URL](https://github.com/openai/codex/blob/091008729d5572712ac08964a9602eb71cf31428/codex-rs/model-provider-info/src/lib.rs), [bearer authentication](https://github.com/openai/codex/blob/091008729d5572712ac08964a9602eb71cf31428/codex-rs/model-provider/src/bearer_auth_provider.rs), [request type](https://github.com/openai/codex/blob/091008729d5572712ac08964a9602eb71cf31428/codex-rs/codex-api/src/common.rs), [HTTP transport](https://github.com/openai/codex/blob/091008729d5572712ac08964a9602eb71cf31428/codex-rs/codex-api/src/endpoint/responses.rs).

`POST https://chatgpt.com/backend-api/codex/responses` with `Authorization: Bearer <access_token>`, `ChatGPT-Account-ID: <account_id>`, `Content-Type: application/json`, and `Accept: text/event-stream`. Codex supplies a User-Agent and `originator: codex_cli_rs`; session/request correlation headers are ancillary. HTTP SSE is implemented upstream, so a WebSocket implementation is unnecessary for the initial proxy.

Small source-aligned text request (model must be an available upstream model):

```json
{
  "model": "<upstream-model>",
  "stream": true,
  "input": [{"type":"message","role":"user","content":[{"type":"input_text","text":"Hello"}]}],
  "tools": [],
  "tool_choice": "auto",
  "parallel_tool_calls": false,
  "reasoning": null,
  "store": false,
  "include": []
}
```

Use streaming upstream for both streaming and buffered downstream APIs. Instructions can be represented as a developer message in input. This payload is derived from the current source serializer, not a live-tested minimum accepted schema.

## Events, usage, and routing feedback

Sources: [SSE parser](https://github.com/openai/codex/blob/091008729d5572712ac08964a9602eb71cf31428/codex-rs/codex-api/src/sse/responses.rs), [rate limits](https://github.com/openai/codex/blob/091008729d5572712ac08964a9602eb71cf31428/codex-rs/codex-api/src/rate_limits.rs).

- `response.created` establishes response identity; `response.output_text.delta` carries `delta`. Output-item/content-part events describe the output envelope.
- `response.completed` is success. `response.failed` contains `response.error`. `response.incomplete` carries `response.incomplete_details.reason` and may include usage; preserve the incomplete outcome. EOF before a terminal event is a failed stream, not success. An SSE connection can fail after HTTP 200.
- Terminal `response.usage` contains `input_tokens`, `output_tokens`, `total_tokens`, optional `input_tokens_details.cached_tokens`, and optional `output_tokens_details.reasoning_tokens`. Missing usage stays unknown; never manufacture zero billed tokens.
- HTTP headers expose `x-codex-primary-used-percent`, `x-codex-primary-window-minutes`, `x-codex-primary-reset-at`, and corresponding `secondary` names. Reset times are Unix seconds. Other metered families use `x-<limit-id>-...` with hyphens. Credits use `x-codex-credits-has-credits`, `x-codex-credits-unlimited`, `x-codex-credits-balance`.
- `codex.rate_limits` SSE JSON contains `rate_limits.primary` and `.secondary`, each with `used_percent`, optional `window_minutes`, `reset_at`; optional `credits`, `plan_type`, `metered_limit_name`/`limit_name`. This is distinct from token usage. Store observation timestamps and model/meter identity when present.
- Routing can avoid an account with a known exhausted, unexpired window and compare cumulative recorded tokens among compatible accounts. Missing or stale remaining-usage observations are unknown, not unlimited. Stream failure/cancellation may leave usage unknown. Do not reroute after delivering text because it would splice different generations.

## Claude invocation constraints

The [official CLI reference](https://code.claude.com/docs/en/cli-reference) documents `-p --output-format stream-json --verbose --include-partial-messages` for streaming output. `--input-format stream-json` represents incoming user turns; that documentation does not establish that arbitrary assistant history can be supplied. Do not silently concatenate historical roles or pretend such input preserves their semantics. `--resume` accepts an absolute transcript JSONL path, but its internal format needs separate verification before using it for stateless request reconstruction.

`--safe-mode` retains authentication while suppressing customizations, unlike bare mode. Managed policy commands can still apply. `--tools ""` disables built-in tools; MCP is separately controlled with `--strict-mcp-config --mcp-config '{"mcpServers":{}}'`. `--setting-sources ""`, an isolated working/config directory, and `--no-session-persistence` reduce inherited state. `--system-prompt-file` replaces the default system prompt. Account credentials must remain available through the selected subscription authentication mechanism.

## Verification boundary

Authentication and inference details above were checked against source only. Claude subprocess behavior and `claude-universal` require separate source/documentation inspection. Subscription service availability, account-specific model access, and live token rotation remain unverified until account-backed smoke tests are explicitly arranged.
