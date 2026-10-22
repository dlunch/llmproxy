# llmproxy

A Rust subscription-backed LLM proxy with separate API, gateway, and provider modules.

- `POST /v1/chat/completions` and `POST /v1/responses`, streaming or collected.
- Codex OAuth and HTTP inference directly, without invoking Codex CLI.
- Claude through the official [Claude Code](https://code.claude.com/docs/en/overview) executable using `claude -p`.
- Client-defined function calls/results and native web search where the provider supports them.
- Accounts with multiple API tokens and IP CIDRs; PostgreSQL request metadata and token usage belong to the account.
- JSON request/response and operational logs on stdout; Prometheus at `/metrics` and liveness at `/health`.

## Run

Install Rust and use a PostgreSQL 17 or later database:

```sh
export DATABASE_URL='postgres://user:password@127.0.0.1:5432/llmproxy'
cp config.example.toml config.toml
cargo run --locked
```

Edit provider paths before starting. `LLMPROXY_CONFIG` selects a configuration file; the default is `config.toml`. Database migrations run on startup. Register account-owned providers and their available models using the SQL below. No provider call is made until an authenticated client requests inference.

The server defaults to `127.0.0.1:8080`. Operational endpoints share the listener. For remote access, place it behind your normal TLS/network boundary. CIDR authentication always uses the TCP peer, never `X-Forwarded-For` or `X-Real-IP`; behind a reverse proxy the peer is the proxy, so use API tokens rather than allowlisting a shared proxy address.

## Container and Kubernetes

GitHub Actions publishes `ghcr.io/dlunch/llmproxy` for `linux/amd64` and `linux/arm64`, built on matching native runners. Pull requests build without publishing. Main pushes publish `latest` and `sha-<full-commit>`; `v*` tags publish the tag name and commit tag. Authentication uses the workflow's `GITHUB_TOKEN` with package write permission.

The image runs as UID/GID 1000 and bundles the unmodified official Claude Code binary. Each build resolves the official latest release and verifies its download against the official SHA-256 manifest. The remote version file participates in Docker's cache key, so a new release invalidates the download layer. Update Claude by rebuilding and deploying a new image. Subscription credentials remain in PostgreSQL. Use `listen = "0.0.0.0:8080"` in the mounted configuration and a database address reachable from the container:

```sh
docker run --rm -p 127.0.0.1:8080:8080 \
  --env DATABASE_URL \
  --mount type=bind,src="$PWD/config.toml",dst=/etc/llmproxy/config.toml,readonly \
  ghcr.io/dlunch/llmproxy:latest
```

Build locally with `docker build -t llmproxy .`. For Kubernetes, create a database connection Secret using your usual Secret management process, then install the [Helm chart](charts/llmproxy/README.md):

```sh
helm upgrade --install llmproxy ./charts/llmproxy \
  --set database.existingSecret=llmproxy-database \
  --set image.tag='sha-<full-commit>'
```

The chart supports multiple replicas and rolling updates. PostgreSQL row locks serialize credential refresh across replicas. The bundled executable requires no persistent volume or startup installer. See the [chart README](charts/llmproxy/README.md) for deployment and image pull settings.

## Accounts and client authentication

There is no management CLI, dashboard, or management endpoint. Seed test accounts, tokens, and networks directly in PostgreSQL; a dashboard can manage these tables later. API tokens are SHA-256 digests of high-entropy random bearer tokens, not passwords.

```sql
INSERT INTO accounts (id, label)
VALUES ('11111111-1111-4111-8111-111111111111', 'development');

-- Replace the example plaintext before using this outside a local test.
INSERT INTO api_tokens (id, account_id, label, token_hash)
VALUES (gen_random_uuid(), '11111111-1111-4111-8111-111111111111',
        'test token', sha256(convert_to('local-test-token', 'UTF8')));

-- Add more tokens or CIDRs to the same account as needed.
INSERT INTO account_ip_ranges (id, account_id, network)
VALUES (gen_random_uuid(), '11111111-1111-4111-8111-111111111111',
        '127.0.0.1/32');
```

Without an Authorization header, a matching CIDR authenticates the request. Among overlapping enabled accounts, the longest prefix wins; identical CIDRs cannot belong to multiple accounts. IPv4-mapped IPv6 peers are normalized. A supplied malformed, invalid, or revoked bearer token fails authentication even on an allowed IP. Disabled accounts cannot authenticate by either method.

```sql
UPDATE api_tokens SET revoked_at = now() WHERE label = 'test token';
UPDATE accounts SET enabled = false
WHERE id = '11111111-1111-4111-8111-111111111111';
```

## Provider subscription setup

Each configured provider instance belongs to one proxy account. Bind its TOML `id` before provisioning credentials or making requests:

```sql
INSERT INTO provider_instances (id, account_id)
VALUES ('codex-main', '11111111-1111-4111-8111-111111111111'),
       ('claude-main', '11111111-1111-4111-8111-111111111111');
```

An account can own multiple instances of the same kind: add another TOML provider with a distinct ID, its own subscription credentials in PostgreSQL, a matching `provider_instances` row, and available model mappings. Only instances owned by the authenticated account are visible to routing policies. Unbound configuration entries cannot serve client requests, and request records enforce the account/provider ownership relationship in PostgreSQL.

Register the models available to each subscription. `model` is the client-facing alias; `upstream_model` is the provider model name. Multiple providers can serve the same alias. These example names are not an entitlement check.

```sql
INSERT INTO provider_models (provider_id, model, upstream_model)
VALUES ('codex-main', 'codex', 'gpt-5.4'),
       ('claude-main', 'claude', 'claude-sonnet-4-6');
```

Model mappings are read from PostgreSQL on each request. Adding or deleting a row changes availability without restarting the service; TOML contains only transport/provider configuration.

### Automatic OAuth refresh

While the service is running, configured providers belonging to enabled accounts are checked at startup and every minute, even without inference traffic. Access tokens are refreshed within five minutes of expiry, or after 24 hours without a successful token update. Credentials without an access-token expiry are refreshed hourly. These checks call only OAuth endpoints and do not generate billable model requests.

PostgreSQL row locks coordinate refresh with inference and other replicas; shutdown waits for in-flight refreshes to be saved. Failed attempts are retried on the next check. Revoked refresh tokens and absolute login expiry still require a new login, and credentials cannot be maintained while the service is stopped.

### Codex

`providers::codex::CodexAuth` exposes `start_device_login`, `poll_device_login`, and `refresh`. It uses the same device-authentication entrypoint and client ID as the Codex CLI; no CLI subprocess is needed. A future dashboard can call these Rust APIs. `DeviceLogin` contains the verification URL and user code. Polling yields pending, complete credentials, or expired. Refreshes are serialized per provider across replicas with a PostgreSQL row lock and persist rotated credentials before inference; shutdown drains any in-flight refresh.

For manual tests, provision credentials into the provider row using SQL. `account_id` here is the **upstream ChatGPT account ID**, distinct from a proxy account UUID:

```sql
INSERT INTO provider_credentials
    (provider_id, access_token, refresh_token, account_id, expires_at)
VALUES ('codex-main', '<access token>', '<refresh token>',
        '<ChatGPT account ID>', '<access token expiry timestamp>');
```

Keep credential provisioning out of request logs and shell history. The authentication implementation follows the [official Codex source](https://github.com/openai/codex/tree/091008729d5572712ac08964a9602eb71cf31428/codex-rs/login/src); device login must be enabled for the upstream account. The proxy uses `https://chatgpt.com/backend-api/codex/responses`, with subscription bearer and account headers. It does not switch to API-key billing or retry submitted inference automatically.

### Claude

The container includes the official executable at `/usr/local/bin/claude`. For a host installation, follow the [official setup instructions](https://code.claude.com/docs/en/setup) and set `executable` in `config.toml` to its absolute path. Establish your subscription login in a private provisioning directory:

```sh
CLAUDE_CONFIG_DIR=/private/claude-login /absolute/path/to/claude auth login
```

Anthropic documents [conditional permission to preinstall unmodified Claude Code](https://code.claude.com/docs/en/legal-and-compliance#can-customers-offer-claude-code-in-their-products). This project is intended for personal, self-hosted use; that preinstallation permission does not authorize collecting other users' Claude credentials or intermediating their subscription usage. Review the linked authentication conditions before adapting it into a third-party service.

Import `.credentials.json` and the login fields from `.claude.json` into the provider's database row. The credentials and OAuth account objects are externally defined JSON documents; the user ID and onboarding flag have typed columns. Use a secure database client with bound parameters for real secrets; the following shows the row shape with placeholders:

```sql
INSERT INTO claude_credentials
    (provider_id, credentials, oauth_account, user_id, has_completed_onboarding)
VALUES ('claude-main', '<.credentials.json contents>'::jsonb,
        '<.claude.json oauthAccount object>'::jsonb, '<.claude.json userID>', true);
```

The service restores this login data into a private temporary configuration directory for each invocation and persists refreshed credentials before completing the request. Calls using the same provider ID are serialized across replicas with a PostgreSQL row lock held until the CLI exits and refreshed credentials are saved. Project settings, conversation history, and caches are not imported into the database.

Each inference gets a private temporary working directory and a cleared process environment. Safe mode preserves subscription login while disabling user customizations; no inherited API-key/provider environment variables are passed. Built-in tools are disabled except `WebSearch` when requested. MCP configuration contains only the proxy's in-process function-schema bridge. Server shell/file tools are never exposed.

Conversation history is resumed from a private temporary JSONL transcript, preserving assistant messages and function results. The transcript is deleted when the request finishes; session persistence is disabled. Function calls are handed back after the complete assistant turn, and the CLI is stopped before executing client functions. Search remains provider-executed. Claude's own agent context/reminders can still differ from a raw model API, and administrator-managed Claude policies still apply.

## Requests

```sh
curl http://127.0.0.1:8080/v1/chat/completions \
  -H 'Authorization: Bearer local-test-token' \
  -H 'Content-Type: application/json' \
  -d '{"model":"codex","messages":[{"role":"user","content":"Hello"}],"stream":true}'

curl http://127.0.0.1:8080/v1/responses \
  -H 'Authorization: Bearer local-test-token' \
  -H 'Content-Type: application/json' \
  -d '{"model":"claude","input":"Find recent Rust news","tools":[{"type":"web_search"}]}'
```

Omit Authorization only from a configured account network. Chat Completions requests use `web_search_options: {}` for native search. Responses accepts `web_search` and `web_search_preview`. Search location/context/domain filter options are not implemented and return an error rather than being ignored.

For client functions, use standard Chat Completions function tools and `assistant.tool_calls` / `tool` messages, or Responses `function` tools and `function_call` / `function_call_output` input items. The client executes the function; send its result in the next request together with the conversation history. The proxy does not persist Responses objects or support `previous_response_id` retrieval/chaining.

The initial contract is text input/output, function tools, and search. Unsupported fields or modalities fail explicitly. Codex supports automatic/required/named function selection and strict schemas. Claude supports `tool_choice` auto/none; forced choices and strict function schemas are rejected. Sampling and token-limit options are rejected when the subscription provider cannot honor them. Streaming failures never become successful terminal events; length-limited Responses use `response.incomplete`.

Opaque reasoning items are not retained across requests. Function-call transport is covered by local fixtures, but equivalent reasoning context and live continuation across all Codex models have not been established.

## Routing and extension points

`core::Provider` normalizes provider events. `gateway::RoutingPolicy` receives the full normalized request, account token totals, provider usage/quota state, and explicit routes. Register your own trait implementation in the composition layer. API adapters consume the same gateway; they do not contain provider-specific selection logic.

The default policy matches model aliases exactly, excludes exhausted fresh quota windows, then chooses the provider with the lowest accumulated usage, with deterministic ties. Account usage is available to custom policies. No model substitution occurs unless explicitly configured under an alias. Unknown or stale quota stays unknown; five-minute-old snapshots and expired windows do not block selection. Codex reports window snapshots through headers/events. Claude's returned five-hour and seven-day window observations are collected when available; missing windows remain unknown. Exhausted Claude base windows exclude the provider while fresh even if Claude permits extra paid usage. Routing is advisory, not a reservation or strict spending limit.

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

`provider_statistics` derives per-instance statistics from request records, including instances with no requests. Token totals sum only reported usage and stay null if no usage is known; `missing_usage_requests` exposes unreported usage. Latency averages finished requests in milliseconds. The view also exposes `running_requests`. Counters are derived rather than incremented separately, so concurrent completions cannot lose updates.

Stdout uses tracing's JSON format. Request/response body fields contain serialized JSON; streaming responses log each event, correlated by `request_id` and `account_id`. Request bodies and function arguments/results can contain sensitive application content, as requested; authentication headers and provider credentials are never added by the logger. Configure stdout retention/access in your deployment. `RUST_LOG` controls filtering; the default includes `llmproxy=info`.

Prometheus exposes inference outcome counters, active requests, latency, provider errors, and observed token counters. Labels contain configured provider IDs and fixed outcomes/directions, not account IDs, arbitrary model input, or secrets. Failed authentication is rejected before provider accounting.

## Verification

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --locked
helm lint charts/llmproxy --set database.existingSecret=llmproxy-database
python3 tests/helm_chart.py
# Use a disposable PostgreSQL database; tests isolate their tables in random schemas.
export TEST_DATABASE_URL=postgres://user:password@127.0.0.1:5432/llmproxy_test
cargo test --locked -- --include-ignored --skip actual_cli_roundtrips --skip proxy_latency_benchmark
# Optional: exercise the actual Claude executable against a fake local API.
CLAUDE_TEST_EXECUTABLE=/absolute/path/to/claude \
  cargo test --locked --test claude_provider actual_cli_roundtrips -- --ignored
```

Subprocess fixtures require Python 3. Chart checks require Helm, Python 3.11+, and PyYAML. Database-dependent tests are explicitly ignored unless selected. The actual-Claude test requires both `TEST_DATABASE_URL` and `CLAUDE_TEST_EXECUTABLE`; it runs the executable against a fake local API and does not contact a real subscription account. Other provider tests use local HTTP/subprocess fixtures. See the QA report for the actual checks performed and account-dependent limitations. Replicas must use the same provider ID for a shared upstream credential so that PostgreSQL credential locks coordinate them.

Run the request-path latency benchmark separately with a disposable `TEST_DATABASE_URL`:

```sh
cargo test --release --locked --test api_integration proxy_latency_benchmark -- --ignored --nocapture
```

It measures first-frame and complete-response latency for both APIs, with 128 KiB of input, 2,048 synthetic output deltas, and one or four concurrent requests. It includes PostgreSQL and in-process API handling; it excludes live upstream latency, network transport to the client, and production logging sinks.
