# Rust LLM Proxy PRD

Status: Requirements clarified by the user; proceed with design and implementation.

## Purpose

Provide authenticated clients with one LLM endpoint, routing requests across subscription-backed providers according to the requested model and usage conditions. Implement in Rust with PostgreSQL persistence and Prometheus observability.

## Confirmed requirements

- Separate API, gateway, and provider responsibilities behind pluggable interfaces.
- Keep gateway logic independent of HTTP and provider implementations.
- Select providers belonging to the authenticated account using the requested model and additional conditions, including remaining usage and accumulated tokens. Expose account/provider-level request and token usage statistics from persisted records.
- Implement Codex and Claude subscription authentication first.
- Invoke the official Claude Code executable using `claude -p`; the deployment bundles its verified architecture-specific release.
- Accounts own multiple API tokens, allowed IP CIDRs, and provider instances, including multiple instances of the same provider type; requests, usage, and history belong to the account. Disabling an account disables every credential.
- Authenticate clients with API tokens stored as hashes in PostgreSQL, or allow tokenless access from account-owned IP CIDR networks stored in PostgreSQL. Use the direct TCP peer address; never trust forwarded headers by default. A supplied invalid token remains an authentication failure even from an allowed network. Token issuance and management belong to a future web dashboard; implement no management CLI or dashboard now. Seed test tokens manually with SQL.
- Support both OpenAI-compatible Chat Completions and Responses APIs.
- Support provider-native web search and client-defined function tools, including streamed calls and client-supplied tool-result roundtrips. The proxy does not execute arbitrary host tools.
- Implement Codex subscription integration directly using the Codex CLI authentication entrypoint/protocol, without running Codex CLI or app-server.
- Expose Prometheus metrics.
- Persist only account-level request metadata and usage in PostgreSQL; the user explicitly excluded request/response/tool bodies from database history.
- Write request/response bodies and operational events to stdout in JSON format. Include request/account correlation, exclude authentication headers and subscription credentials, and add no separate audit store.

## Initial implementation scope

- A single service with modular Rust interfaces; plugins are registered implementations, without dynamic library loading.
- Streaming and non-streaming adapters for both Chat Completions and Responses.
- A replaceable routing policy receives the normalized request, account usage totals, account-owned provider routes, provider quota snapshots, and locally accumulated provider usage.
- Explicit model-to-provider mappings prevent accidental substitution of a different model. Multiple eligible providers can serve one configured alias.
- Default policy matches explicitly configured model aliases among the account's registered providers, excludes exhausted known limits, and prefers the eligible provider with the lowest locally recorded token usage. Tie breaking is deterministic.
- Provider-reported quota windows, reset times, snapshot freshness, and unknown availability remain distinct from local token totals. Unknown remaining usage is not zero or unlimited. The exact telemetry available from each provider must be verified during integration.
- Document manual SQL insertion and revocation of test API-token hashes. No token-management CLI or public management endpoints in this release.
- PostgreSQL records request ID, token identity, requested model, selected provider/model, timestamps, outcome, latency, and reported token counts. Missing usage remains unknown. Prompt and response bodies are not persisted in PostgreSQL; they are logged to stdout as requested.
- Metrics cover request outcomes, latency, active requests, provider failures, and token usage, with bounded labels. Credentials, prompts, and client token IDs are not metric labels.
- Operational setup includes schema migrations, example configuration, provider login/setup instructions, and local PostgreSQL startup instructions.

## End-to-end behavior

1. An operator creates an account, binds provider instances to it, configures matching instance IDs/model routes, establishes subscription login, and manually inserts a test API-token hash into PostgreSQL.
2. A client sends a model request with a bearer token or connects without a token from an account-owned CIDR.
3. The API adapter authenticates and validates the supported request contract before invoking the gateway.
4. The gateway selects an eligible provider using the routing policy and current usage context.
5. The provider returns normalized content events, terminal status, and usage when available; the API adapter encodes the client protocol.
6. Request state and usage are persisted, and metrics are updated for success, failure, cancellation, or timeout.
7. Invalid or revoked credentials are rejected before inference. Unsupported models/options and exhausted known limits return explicit errors.

## Reliability and isolation

- Bound request size, subprocess output buffering, concurrency, and execution time.
- Cancel provider work when clients disconnect; clean up subprocesses and finalize request state.
- Do not silently report successful completion after upstream failure or truncated output.
- Avoid automatic retries that can duplicate inference after output has begun.
- Run CLI inference with controlled working directories and settings, without exposing server files or executing client-requested host tools.
- Preserve subscription authentication. In particular, Claude's documented `--bare` mode skips OAuth credentials and is not a substitute for isolating a subscription-backed CLI invocation.
- Keep provider credentials out of API responses, request logs, metrics, and version control.
- Concurrent accounting must not lose usage updates. Define reconciliation of interrupted request records during architecture design.
- Fail authentication closed if PostgreSQL cannot validate credentials. Define and test persistence-failure behavior around provider execution.

## Data model

- Accounts: ID, label, enabled status, creation timestamp.
- API tokens: ID, account reference, label, digest, creation timestamp, revocation timestamp.
- Account IP ranges: ID, account reference, unique CIDR network, creation timestamp.
- Provider instances: stable instance ID and owner-account reference in PostgreSQL. A single account can own several Codex or Claude instances, each with its own credential/configuration identity. Model routes and executable/transport configuration remain typed server configuration.
- Provider statistics: request outcomes and reported input/output tokens derived from request records, grouped by account and provider instance; no duplicated counter ledger.
- Requests: account identity and token reference or allowed IP range/client IP, requested and selected model/provider, lifecycle timestamps, terminal outcome, sanitized error category, nullable token counts.
- Usage snapshots: provider reference, observed time, quota window/reset information where supplied. Keep externally defined payloads only if necessary; favor typed columns for routing fields.

## Scope boundaries for the initial release

- No administration web UI, billing system, or dynamic plugin loader.
- Text generation, streaming, native web search, and client-defined function calls/results are in scope. Structured output and multimodal input remain unsupported and are rejected explicitly.
- Provider CLI agents must not execute tools on the proxy host for untrusted API clients.
- No fabricated quota figures or performance guarantees.

## Open questions

- Text, web search, and client-defined function tool roundtrips are confirmed scope; reject unsupported options explicitly.
- Codex authentication must use the CLI authentication entrypoint directly. Verify the actual upstream source before implementing login, token refresh, and inference. Initial credentials may be provisioned manually; expose reusable provider authentication functionality without a management CLI.
- ? Deployment target and performance expectations; no numerical SLA is assumed.

## Acceptance criteria

- An integration test manually inserts a token hash, authenticates a request, selects a fake provider, receives its response/stream, and verifies PostgreSQL records and Prometheus output.
- Tests demonstrate account/provider isolation, multiple instances of the same provider type, provider-level usage statistics, and changed selection from usage conditions, including model filtering, unknown/exhausted quota, and accumulated-token changes.
- Revoked/invalid tokens do not reach providers.
- Provider contract tests cover incremental content, usage, authentication errors, malformed output, timeout, and cancellation using controlled subprocess/transport fixtures.
- PostgreSQL integration tests cover migrations, concurrent usage updates, and request lifecycle persistence.
- Verify real Codex and Claude subscription calls only when an explicitly configured test account is available; report fixture-only verification clearly.
- Run formatting, linting, and automated tests; document observed limitations without claiming unperformed checks.

## Research sources

- https://code.claude.com/docs/en/headless: print mode, structured streaming, subprocess lifecycle, and bare-mode authentication behavior.
- https://github.com/openai/codex/tree/091008729d5572712ac08964a9602eb71cf31428/codex-rs/login/src: verified direct authentication protocol; app-server is not used.
- https://developers.openai.com/siwc/token-sharing-open-source/codex-app-server: an officially documented ChatGPT-plan OAuth integration route; do not assume account entitlement from a model catalog.
