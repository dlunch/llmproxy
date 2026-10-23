# llmproxy

A Rust subscription-backed LLM proxy with separate API, gateway, and provider modules.

- `POST /v1/chat/completions` and `POST /v1/responses`, streaming or collected.
- Codex OAuth and HTTP inference directly, without invoking Codex CLI.
- Claude through the official [Claude Code](https://code.claude.com/docs/en/overview) executable using `claude -p`.
- Client-defined function calls/results and native web search where the provider supports them.
- Accounts with multiple API tokens and IP CIDRs; PostgreSQL request metadata and token usage belong to the account.
- Email/password management sessions, signup codes, provider login, routing configuration, and per-token metrics.
- An English static dashboard served by a separate Nginx image, calling the API directly from the browser.
- JSON request/response and operational logs on stdout; Prometheus at `/metrics` and liveness at `/health`.

## Run

Install Rust and use a PostgreSQL 17 or later database:

```sh
export DATABASE_URL='postgres://user:password@127.0.0.1:5432/llmproxy'
cp config.example.toml config.toml
cargo run --locked
```

Set `claude_executable` to the absolute path of the Claude binary. `LLMPROXY_CONFIG` selects a configuration file; the default is `config.toml`. TOML contains host settings; accounts, provider instances, and model routes are managed through the API and stored in PostgreSQL. Database migrations run on startup. OAuth setup and maintenance do not submit inference requests.

The server defaults to `127.0.0.1:8080`. Operational endpoints share the listener. For remote access, place it behind your normal TLS/network boundary. CIDR authentication always uses the TCP peer, never `X-Forwarded-For` or `X-Real-IP`; behind a reverse proxy the peer is the proxy, so use API tokens rather than allowlisting a shared proxy address.

## Container and Kubernetes

GitHub Actions builds `ghcr.io/dlunch/llmproxy` and `ghcr.io/dlunch/llmproxy-dashboard` independently for `linux/amd64` and `linux/arm64`, on matching native runners. Pull requests build without publishing. Main pushes publish `latest` and `sha-<full-commit>` for both images; `v*` tags publish the tag name and commit tag. Authentication uses the workflow's `GITHUB_TOKEN` with package write permission.

The API image runs as UID/GID 1000 and bundles the unmodified official Claude Code binary. Each build resolves the official latest release and verifies its download against the official SHA-256 manifest. The remote version file participates in Docker's cache key, so a new release invalidates the download layer. Update Claude by rebuilding and deploying a new image. Subscription credentials remain in PostgreSQL. Use `listen = "0.0.0.0:8080"` in the mounted configuration and a database address reachable from the container:

```sh
docker run --rm -p 127.0.0.1:8080:8080 \
  --env DATABASE_URL \
  --mount type=bind,src="$PWD/config.toml",dst=/etc/llmproxy/config.toml,readonly \
  ghcr.io/dlunch/llmproxy:latest
```

Build the API locally with `docker build -t llmproxy .`. The dashboard uses [unprivileged Nginx](https://github.com/nginx/docker-nginx-unprivileged) to serve only static files. For local development, keep the example API configuration's `dashboard_origin = "http://127.0.0.1:8081"` and `cookie_secure = false`, then run:

```sh
docker build -t llmproxy-dashboard ./dashboard
docker run --rm --read-only --tmpfs /tmp \
  -p 127.0.0.1:8081:8080 llmproxy-dashboard
```

Open `http://127.0.0.1:8081`. Its default `config.json` points directly to the API at `http://127.0.0.1:8080`. The dashboard supports signup/login, account settings, provider login/configuration, routing, and API tokens with usage statistics. Token secrets are shown once and are not saved in browser storage. To change the API address outside Helm, mount a `config.json` containing `{"apiUrl":"https://api.example.com"}` at `/usr/share/nginx/html/config.json`.

For Kubernetes, create a database connection Secret using your usual Secret management process and choose two public origins under the same site. Set their actual addresses when installing the [Helm chart](charts/llmproxy/README.md); the following domains are examples:

```sh
helm upgrade --install llmproxy ./charts/llmproxy \
  --set database.existingSecret=llmproxy-database \
  --set dashboard.apiUrl=https://api.example.com \
  --set dashboard.origin=https://dashboard.example.com \
  --set image.tag='sha-<full-commit>' \
  --set dashboard.image.tag='sha-<full-commit>'
```

The chart creates separate API and dashboard Deployments and ClusterIP Services, each on port 8080. Route the API domain to the `llmproxy` Service and the dashboard domain to `llmproxy-dashboard` through your existing HTTPS routing. The API domain serves management, completion, and streaming requests; the browser calls it directly. Nginx does not proxy API traffic. Both deployments support multiple replicas and rolling updates. PostgreSQL row locks serialize credential refresh across API replicas; neither application needs persistent volumes or sticky sessions.

## Accounts and management API

Before upgrading an existing deployment, remove all `[[providers]]` entries and set the host's `claude_executable`; the old configuration is rejected. The schema update retains credentials and request history, inferring provider kind only when exactly one credential table identifies it. Providers with an unresolved kind stay inactive until an operator assigns it. Existing accounts need a normalized unique email and Argon2 PHC password hash before management login; existing inference tokens continue to work.


Issue a single-use signup code against the initialized database:

```sh
psql "$DATABASE_URL" -XAt -v ON_ERROR_STOP=1 -f scripts/issue-signup-code.sql
```

The script prints the code once and stores only its SHA-256 digest. Signup consumes the code atomically with account creation. Email is the login identifier; no email delivery or verification is required. Passwords are stored as Argon2 hashes.

Create a private `signup.json` containing `email`, `password` (8–1024 bytes), `label`, and `signup_code`. Create a private `login.json` containing the same `email` and `password`, then register and sign in:

```sh
curl http://127.0.0.1:8080/management/accounts \
  -H 'Content-Type: application/json' -H 'X-LLMProxy-Management: 1' \
  --data-binary @signup.json
curl http://127.0.0.1:8080/management/sessions \
  -H 'Content-Type: application/json' -H 'X-LLMProxy-Management: 1' \
  --data-binary @login.json -c session.cookies
curl http://127.0.0.1:8080/management/account -b session.cookies
```

Login sets a 24-hour HttpOnly, SameSite=Strict cookie scoped to `/management`; it does not issue an inference token. `cookie_secure` defaults to `true` and requires HTTPS. The example configuration disables it for local HTTP development. All management mutations, including signup and login, require `X-LLMProxy-Management: 1`. Management CORS allows credentials from only the configured `dashboard_origin`; inference endpoints do not enable CORS. Use HTTPS sibling domains so the session cookie remains same-site. Inference bearer tokens and IP CIDRs do not authenticate management endpoints.

Authenticated endpoints are scoped to the signed-in account. Prefix each path below with `/management` and send the session cookie:

| Method and path | Request or result |
| --- | --- |
| `GET /account`, `PATCH /account` | Read account; update `{"label":"Personal"}` |
| `DELETE /session` | Revoke the current management session and clear its cookie |
| `GET /api-tokens`, `POST /api-tokens` | List tokens; create with `{"label":"Laptop"}` |
| `DELETE /api-tokens/{id}` | Revoke an inference token |
| `GET /api-tokens/{id}/metrics` | Request outcomes, usage, and execution latency for that token |
| `GET /providers`, `POST /providers` | List providers; register `{"kind":"codex","label":"Main"}` |
| `GET /providers/{id}`, `PATCH /providers/{id}` | Read provider; update `label` or `enabled` |
| `GET /routing`, `PUT /routing` | Read or atomically replace routing strategy and all model routes |

API-token creation returns the plaintext `token` once. Store it securely; subsequent reads return only metadata. Multiple tokens can belong to one account, and revocation preserves request history and metrics. Token metrics sum reported usage, expose `missing_usage_requests`, and include cancelled or failed requests with usage. `mean_execution_latency_ms` averages the persisted request interval; it excludes client authentication and routing, unlike the end-to-end Prometheus request histogram.

For inference, send `Authorization: Bearer <token>`. Without an Authorization header, a matching operator-provisioned CIDR authenticates the request. Among overlapping enabled accounts, the longest prefix wins; identical CIDRs cannot belong to multiple accounts. IPv4-mapped IPv6 peers are normalized. A supplied malformed, invalid, or revoked bearer token fails authentication even on an allowed IP. Disabled accounts cannot authenticate by either method or use management sessions.

## Provider subscription setup

Register a provider through `POST /management/providers` with `kind` set to `codex` or `claude`, a `label`, and optional `enabled` (default `true`). Keep the returned `id` for login and routing. An account can own multiple instances of either kind. Provider metadata never includes subscription credentials; `authenticated` reports whether credentials have been saved, not whether the upstream subscription remains valid.

Start login with `POST /management/providers/{id}/logins`. The response includes the login `id`, `authorization_url`, expiry, and Codex's `user_code`. Open the URL and complete the provider's authorization steps. Submit completion to `POST /management/providers/{id}/logins/{login_id}/complete`:

- **Codex:** send `{}` to poll the device login, respecting `interval_seconds` and `next_poll_at`. A pending attempt returns HTTP 202; successful credential persistence returns 200.
- **Claude:** send `{"code":"<authorization_code#state>"}` with the complete value from the authorization page. The server verifies state and completes the PKCE exchange.

`GET /management/providers/{id}/logins/{login_id}` reports `pending`, `processing`, `completed`, `failed`, or `expired`. Expired or failed completion returns 409; start a new login to retry. Starting a new login replaces the previous attempt but cannot replace an in-flight exchange. Credentials and intermediate secrets remain in PostgreSQL, and concurrent replicas coordinate exchanges and refreshes with row locks. Repeating an already completed login is safe.

Set model aliases with `PUT /management/routing`. This replaces the entire account configuration atomically; include every route to retain:

```json
{
  "strategy": "least_used",
  "routes": [
    {"model": "codex", "provider_id": "<Codex provider id>", "upstream_model": "gpt-5.4", "priority": 0},
    {"model": "claude", "provider_id": "<Claude provider id>", "upstream_model": "claude-sonnet-4-6", "priority": 0}
  ]
}
```

`model` is the client-facing alias; `upstream_model` is the provider model name. Multiple providers can serve the same alias. These example names are not an entitlement check. Only enabled providers owned by the requesting account participate in routing. Complete provider login before sending inference. Provider and route changes apply without restarting the service.

### Automatic OAuth refresh

While the service is running, enabled providers belonging to enabled accounts are checked at startup and every minute, even without inference traffic. At most four maintenance refreshes run at once; shutdown drains started work without starting queued refreshes. Access tokens are refreshed within five minutes of expiry, or after 24 hours without a successful token update. Credentials without an access-token expiry are refreshed hourly. These checks call only OAuth endpoints and do not generate billable model requests.

Fresh credentials are read without an exclusive lock. PostgreSQL row locks coordinate refresh across requests and replicas; credentials are reread under the lock before exchanging a token. Shutdown waits for in-flight refreshes to be saved. Failed attempts are retried on the next check. Revoked refresh tokens and absolute login expiry still require a new login, and credentials cannot be maintained while the service is stopped.

### Codex

Device login uses the same authentication entrypoint and client ID as the Codex CLI, without a CLI subprocess. The authentication implementation follows the [official Codex source](https://github.com/openai/codex/tree/091008729d5572712ac08964a9602eb71cf31428/codex-rs/login/src); device login must be enabled for the upstream account. The proxy uses `https://chatgpt.com/backend-api/codex/responses`, with subscription bearer and account headers. It does not switch to API-key billing or retry submitted inference automatically.

### Claude

The container includes the official executable at `/usr/local/bin/claude`. For a host installation, follow the [official setup instructions](https://code.claude.com/docs/en/setup) and set `claude_executable` in `config.toml` to its absolute path. The management login flow stores the subscription credentials and account metadata needed by the executable.

Anthropic documents [conditional permission to preinstall unmodified Claude Code](https://code.claude.com/docs/en/legal-and-compliance#can-customers-offer-claude-code-in-their-products). This project is intended for personal, self-hosted use; that preinstallation permission does not authorize collecting other users' Claude credentials or intermediating their subscription usage. Review the linked authentication conditions before adapting it into a third-party service.

The host owns refresh tokens and initializes private account metadata for each invocation. Each CLI receives only its access token, allowing calls using the same provider ID to run concurrently. When the CLI requests a replacement token through its OAuth control protocol, the host coordinates refresh under a short PostgreSQL row lock and persists the result before replying. Concurrent refresh requests reuse a newer stored token. Cancellation does not discard an exchange that has already started. Child configuration, conversation history, and caches are not written back into the database.

Each inference gets a private temporary working directory and a cleared process environment. Safe mode preserves subscription login while disabling user customizations; no inherited API-key/provider environment variables are passed. Built-in tools are disabled except `WebSearch` when requested. MCP configuration contains only the proxy's in-process function-schema bridge. Server shell/file tools are never exposed.

Conversation history is resumed from a private temporary JSONL transcript, preserving assistant messages and function results. The transcript is deleted when the request finishes; session persistence is disabled. Function calls are handed back after the complete assistant turn, and the CLI is stopped before executing client functions. Search remains provider-executed. Claude's own agent context/reminders can still differ from a raw model API, and administrator-managed Claude policies still apply.

## Requests

```sh
curl http://127.0.0.1:8080/v1/chat/completions \
  -H "Authorization: Bearer $LLMPROXY_API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"model":"codex","messages":[{"role":"user","content":"Hello"}],"stream":true}'

curl http://127.0.0.1:8080/v1/responses \
  -H "Authorization: Bearer $LLMPROXY_API_TOKEN" \
  -H 'Content-Type: application/json' \
  -d '{"model":"claude","input":"Find recent Rust news","tools":[{"type":"web_search"}]}'
```

Set `LLMPROXY_API_TOKEN` to an issued inference token. Omit Authorization only from a configured account network. Chat Completions requests use `web_search_options: {}` for native search. Responses accepts `web_search` and `web_search_preview`. Search location/context/domain filter options are not implemented and return an error rather than being ignored.

For client functions, use standard Chat Completions function tools and `assistant.tool_calls` / `tool` messages, or Responses `function` tools and `function_call` / `function_call_output` input items. The client executes the function; send its result in the next request together with the conversation history. The proxy does not persist Responses objects or support `previous_response_id` retrieval/chaining.

The initial contract is text input/output, function tools, and search. Unsupported fields or modalities fail explicitly. Codex supports automatic/required/named function selection and strict schemas. Claude supports `tool_choice` auto/none; forced choices and strict function schemas are rejected. Sampling and token-limit options are rejected when the subscription provider cannot honor them. Streaming failures never become successful terminal events; length-limited Responses use `response.incomplete`.

Opaque reasoning items are not retained across requests. Function-call transport is covered by local fixtures, but equivalent reasoning context and live continuation across all Codex models have not been established.

## Routing and extension points

`core::Provider` normalizes provider events. `gateway::RoutingPolicy` receives the full normalized request, account token totals, provider usage/quota state, and explicit routes. Register your own trait implementation in the composition layer. API adapters consume the same gateway; they do not contain provider-specific selection logic.

The configured policy matches model aliases exactly and excludes exhausted fresh quota windows. The default `least_used` strategy chooses the provider with the lowest accumulated usage; `priority` chooses the lowest route priority number. Ties are resolved by provider ID. Account usage is available to custom policies. No model substitution occurs unless explicitly configured under an alias. Unknown or stale quota stays unknown; five-minute-old snapshots and expired windows do not block selection. Codex reports window snapshots through headers/events. Claude's returned five-hour and seven-day window observations are collected when available; missing windows remain unknown. Exhausted Claude base windows exclude the provider while fresh even if Claude permits extra paid usage. Quota belongs to the current provider login, so relogin does not reuse an old account's observations. Routing is advisory, not a reservation or strict spending limit.

## Records, logs, and metrics

Requests record account ID, authentication source, model/provider selection, timestamps, outcome, and nullable input/output token counts. There is no separate audit ledger. PostgreSQL does **not** store prompts, responses, function definitions, or tool results. Usage missing from a provider stays null, including interrupted work.

```sql
SELECT account_id, count(*) AS requests,
       sum(input_tokens) AS input_tokens, sum(output_tokens) AS output_tokens
FROM requests GROUP BY account_id;

SELECT provider_id, request_count, succeeded_requests, failed_requests,
       cancelled_requests, timed_out_requests, interrupted_requests,
       input_tokens, output_tokens, missing_usage_requests,
       mean_latency_ms, last_request_at
FROM provider_statistics
WHERE account_id = '11111111-1111-4111-8111-111111111111'
ORDER BY provider_id;

SELECT id, requested_model, provider_id, started_at, finished_at,
       outcome, input_tokens, output_tokens
FROM requests
WHERE account_id = '11111111-1111-4111-8111-111111111111'
ORDER BY started_at DESC;
```

`provider_statistics` derives per-instance statistics from request records, including instances with no requests. Token totals sum only reported usage and stay null if no usage is known; `missing_usage_requests` exposes unreported usage. Latency averages finished requests in milliseconds. The view also exposes `running_requests`.

Routing reads stored `input_tokens`/`output_tokens` totals on `accounts` and `provider_instances`, avoiding a history scan on every request. A database trigger updates both totals atomically with request usage changes, including reported usage on failed or cancelled requests. Unknown usage contributes zero to routing totals while remaining null in request records. Counter updates briefly serialize completions for the same account. The usage-totals migration initializes existing data while holding a request writer lock; it does not discard history. Treat these totals as database-maintained fields.

Stdout uses tracing's JSON format. Inference request/response body fields contain serialized JSON; streaming responses log each event, correlated by `request_id` and `account_id`. Request bodies and function arguments/results can contain sensitive application content, as requested; authentication headers and provider credentials are never added by the logger. Management request bodies are not logged. Configure stdout retention/access in your deployment. `RUST_LOG` controls filtering; the default includes `llmproxy=info`.

Log output runs on a dedicated worker with a bounded 256-record queue and a standard output buffer flushed between batches. Formatting still runs on the caller; a full queue applies backpressure instead of dropping records. Graceful shutdown drains queued output and reports sink failures. The queue bounds record count rather than bytes, and a stalled stdout sink can delay requests or shutdown.

Prometheus exposes inference outcome counters, active requests, latency, provider errors, and observed token counters with provider ID, API-token ID, and authentication-source labels. CIDR requests have an empty token ID. `llmproxy_rejected_requests_total` separately counts authenticated requests rejected before durable admission, such as unavailable routes or exhausted quota; these are absent from request-history metrics. Authentication failures are excluded. Labels never contain token secrets, account IDs, or arbitrary model input.

`llmproxy_stage_duration_seconds` measures `authentication`, `routing`, `credential_read`, `credential_lock`, `credential_refresh`, `provider_setup`, and `first_text`; background work uses separate `maintenance_credential_lock` and `maintenance_credential_refresh` stages. Stage labels are fixed and buckets include millisecond-scale durations.

The request-duration histogram starts at inference handler entry and ends after durable terminal accounting, including authentication and routing. Framework body extraction before handler entry and final client network drain are outside that interval. `first_text` ends at the first nonempty text delta received by the gateway; metadata, empty deltas, and tool-only responses do not count. Provider setup ends when `infer` returns its stream. Stage durations can overlap and should not be summed. Failed authentication contributes an authentication timing but is rejected before provider accounting.

## Verification

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
helm lint charts/llmproxy --set database.existingSecret=llmproxy-database \
  --set dashboard.apiUrl=https://api.example.com \
  --set dashboard.origin=https://dashboard.example.com
python3 tests/helm_chart.py
# Use a disposable PostgreSQL database; tests isolate their tables in random schemas.
export TEST_DATABASE_URL=postgres://user:password@127.0.0.1:5432/llmproxy_test
cargo test --locked -- --include-ignored --skip actual_cli_roundtrips --skip proxy_latency_benchmark
# Optional: exercise the actual Claude executable against a fake local API.
CLAUDE_TEST_EXECUTABLE=/absolute/path/to/claude \
  cargo test --locked --test claude_provider actual_cli_roundtrips -- --ignored
```

For browser integration checks, install Playwright with Chromium in your test environment and run `node tests/dashboard.cjs` with `DASHBOARD_TEST_URL` and a fresh `DASHBOARD_SIGNUP_CODE`. Point the static site's `config.json` at a disposable API/database on a separate origin, and set that API's `dashboard_origin` accordingly. The test creates an account and exercises token, provider, routing, and session flows; OAuth UI responses use local fixtures. Set `DASHBOARD_SCREENSHOTS` to retain desktop/mobile screenshots.

Subprocess fixtures require Python 3. Chart checks require Helm, Python 3.11+, and PyYAML. Database-dependent tests are explicitly ignored unless selected. The actual-Claude test requires both `TEST_DATABASE_URL` and `CLAUDE_TEST_EXECUTABLE`; it runs the executable against a fake local API and does not contact a real subscription account. Other provider tests use local HTTP/subprocess fixtures. See the QA report for the actual checks performed and account-dependent limitations. Replicas must use the same provider ID for a shared upstream credential so that PostgreSQL credential locks coordinate them.

Run the request-path latency benchmark separately with a disposable `TEST_DATABASE_URL`:

```sh
cargo test --release --locked --test api_integration proxy_latency_benchmark -- --ignored --nocapture
```

It measures first-frame, first-text, complete-response latency, and throughput for both APIs at concurrency one and four. Scenarios cover short responses, 128 KiB input with 2,048 synthetic output deltas, and 100,000 existing request records. Each scenario records 50 batches after five warm-ups. Full JSON logging writes to a temporary file and is drained before exit. Measurements include PostgreSQL and in-process API handling; they exclude live upstream latency, client network transport, and the deployment's log collector. Run benchmarks separately from builds and other tests.
