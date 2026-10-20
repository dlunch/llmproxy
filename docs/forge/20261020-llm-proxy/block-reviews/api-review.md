# API Implementation Self-Review

Scope: `src/api.rs`, `src/api/output.rs`, and `tests/api_integration.rs`.

## Behavior checked

- Both Chat Completions and Responses support collected and streaming text, client function calls/results, and provider-native web search. Function arguments are accumulated from deltas; call-only Responses contain function items without a fabricated assistant message.
- Responses preserve stable response/item IDs, increasing event sequence numbers, item/content/function lifecycle events, citations, and incomplete termination on output length limits. Chat Completions preserve `tool_calls` and `length` finish reasons and optional usage chunks.
- Request parsing rejects unsupported semantic fields and malformed roles/options. Tool results must reference an earlier unanswered function call. Standard returned function-call items can be supplied in the next request with the corresponding result. Returned Chat citation metadata and completed Responses search trace items can also be replayed; the assistant text is preserved as conversational context.
- Initial provider errors are awaited before committing SSE headers. Errors after output begins emit failure events and never a successful terminal marker.
- Authentication resolves an enabled account from a token hash or an account CIDR matched against the canonical TCP peer. Forwarded IP headers are ignored. Any supplied invalid Authorization header is rejected without an IP fallback. API code has no static IP allowlist.
- Both tokens and CIDRs of the same account produce account-scoped persisted usage. Missing usage is not synthesized as zero. Output memory is bounded by the gateway's output limit.

## Verification performed

`cargo test --test api_integration -- --ignored` against a local PostgreSQL test database: **7 passed**, using isolated schemas. Coverage includes both protocols, SSE ordering and failures, function roundtrips, web-search citations, SQL authentication and usage records, Prometheus output, revocation, disabled accounts, multiple account credentials, IPv4-mapped peers, spoofed forwarded headers, exact web-search output replay, and structured log capture with authentication-secret exclusion and correlated setup/collected failures.

Initial test compilation failed before the API module existed; subsequent implementation compiled and the integration suite passed. No production-only helpers or type derives were added to accommodate tests.

## Request/response logging

Accepted request JSON is logged once after gateway request-ID assignment. Collected responses are logged once; SSE data is logged as it is emitted, including terminal markers. The `body_json` field contains a JSON-encoded string (or the Chat SSE `[DONE]` marker), and each accepted request/response event includes `request_id` and `account_id`. Safe API errors log their response body. Accepted setup/collected failures include the same request/account identifiers; errors before gateway admission do not have a gateway request ID. Authorization headers, cookies, raw bearer tokens, and provider credentials are never included. PostgreSQL remains limited to account, request metadata, and usage records; no audit-body table or separate log pipeline was introduced.

## Limits

Only default native web-search options are accepted; custom search filters, location, and context-size options return validation errors. Stored Responses, previous-response IDs, images/audio, and arbitrary server-side tools remain unsupported. Function execution belongs to the client. Tests use a fake provider with the real gateway, PostgreSQL, and API router; they do not verify live subscription behavior or third-party SDK compatibility.
