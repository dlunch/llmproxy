# Implementation review

Scope: API request/output conversion, token and allowed-IP authentication, application composition/configuration, and their integration with provider/gateway lifecycle. Codex changes additionally received cross-review from the architecture agent. Review covered the current working-tree implementation, not an isolated commit.

## Findings and disposition

| Severity | Finding | Resolution and evidence |
|---|---|---|
| High | Cancelling a request during an OAuth refresh could drop the rotated credential before PostgreSQL persistence. | Credential refresh finishes in an owned task. The existing PostgreSQL/HTTP fixture cancels an active refresh and verifies the replacement token is stored. |
| High | Spawning refresh tasks before acquiring the provider mutex let repeated cancelled requests accumulate detached waiters. | The inference future now acquires an owned mutex guard before spawning. Waiting callers remain cancellable; only the lock holder can create a detached credential task. |
| High | Draining HTTP connections alone did not drain detached credential refresh or gateway finalization tasks before Tokio runtime shutdown. | Gateway lifecycle tasks are tracked and drained, then provider shutdown hooks complete. Codex shutdown waits for its credential mutex. Main invokes gateway shutdown after Axum drains. The existing refresh fixture now checks persistence immediately after the provider shutdown hook returns. |
| Medium | An assistant web-search response could not be reused in the next request: Chat rejected its own `annotations` field, and Responses rejected its own `web_search_call` items. | API input accepts typed citation metadata and completed informational search history, preserving the assistant text. The API agent extended the existing search test to replay the returned message/output. |
| Medium | Codex citation offsets from a later output text part pointed into earlier commentary after the proxy concatenated text. | The provider tracks each `(output_index, content_index)` start and character count, adjusts citation offsets, and rejects unknown or invalid spans. The existing tool/search fixture includes preceding Korean text and an emoji; the later two-character citation maps to aggregate offsets 2–4. |

No additional blocking defect was identified in the reviewed API authentication or startup configuration paths after these changes. Token lookup checks account enablement and credential revocation. Allowed-IP authentication uses the direct socket peer and normalizes IPv4-mapped IPv6 addresses; forwarding headers do not grant trust. Malformed or invalid supplied bearer credentials do not fall back to IP authentication. Startup wires socket peer information, fixes Codex service URLs, disables HTTP redirects, validates provider/route configuration, and does not log credential values.

## Verification observed by this reviewer

- Codex local HTTP plus isolated-schema PostgreSQL integration suite: **2 passed, 0 failed, 0 ignored** after the shutdown and citation changes.
- The suite covers device pending/poll interval/exchange/refresh, concurrent refresh, credential rotation on cancellation/shutdown, request headers and role mapping, native function and search serialization, function argument deduplication, citations, streamed Unicode/CRLF framing, missing/failed/malformed terminals, output bounds, quota windows including usage above 100%, unsupported options, and active-stream cancellation.
- Formatting checks passed for the core, gateway, Codex provider, and Codex integration-test files after the final lifecycle changes; final repository formatting and all-target checks remain part of parent QA.
- An all-target Clippy run during concurrent implementation reported remaining Claude-provider collapsible-if warnings. This review does not claim that run passed; final QA must report its own completed result.
- A full process-level SIGTERM test with an active credential refresh was not performed. The implemented shutdown chain was inspected and the provider drain operation was exercised through the fixture.

## Protocol evidence and limits

The [Responses event reference](https://developers.openai.com/api/reference/resources/responses/streaming-events) describes citation indexes as character positions. Offset normalization therefore counts Unicode characters rather than UTF-8 bytes. The fixture establishes the proxy's mapping behavior; real subscription-backed citation output was not exercised.

No real Codex or Claude account was used. Local fixtures prove proxy behavior against the inspected protocol, not current account eligibility, model availability, subscription policy, or live interoperability. OAuth refresh serialization remains process-local, as documented for the initial single-instance deployment. An abrupt process crash or loss of database availability during remote token rotation cannot be made atomic with the upstream OAuth service.

The normalized request/event interface does not currently retain opaque reasoning items. OpenAI's [stateless conversation guidance](https://developers.openai.com/api/docs/guides/conversation-state) recommends preserving encrypted reasoning output for reasoning-model continuation. The fixtures verify client function-call/result transport, but do not establish equivalent reasoning context or live acceptance for every Codex reasoning model. No live rejection was observed or claimed.

## Independent account-owned provider and statistics review

The architecture reviewer independently inspected `src/store.rs`, `src/gateway.rs`, `migrations/0001_initial.sql`, and `tests/gateway_integration.rs` after the ownership changes were ready. No additional blocking finding was identified.

`provider_states` intersects configured provider IDs with the authenticated account's provider instances. The quota lookup uses only those eligible IDs. Gateway filters routes against these states before calling the replaceable routing policy, so a policy sees no foreign provider route or usage/quota state. The selected provider is resolved from this filtered route list. A composite request foreign key enforces `(account_id, provider_id)` ownership during insertion, before inference starts. Multiple instances of the same runtime provider implementation remain independently routable and accounted.

The statistics view preserves provider instances with no requests, counts request IDs rather than outer-join rows, retains nullable token totals, counts missing-usage requests, and computes mean latency only for completed timestamp pairs. The requests join includes both instance and owner, and primary/composite keys prevent duplicated accounting rows. Credentials and quota rows reference registered database instances; TOML remains responsible for runtime implementation/settings.

The extended integration fixture was inspected: a custom policy asserts all received routes/states belong to its account, two accounts sharing an alias select their own instances, a foreign-only alias is rejected, a misattributed request insert fails, and each instance's successful request statistics are checked. Execution evidence is reported by the implementation/QA agent separately; this independent review did not rerun the database suite. Operator provisioning is trusted, and authenticated Principals are produced by the existing token/IP lookup boundary.
