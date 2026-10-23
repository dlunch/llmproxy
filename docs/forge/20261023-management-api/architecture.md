# Management API architecture

## Design and boundaries

Extend the existing Axum application with `/management` HTTP routes. PostgreSQL owns account identity, management sessions, inference API tokens, provider instances, routing, and OAuth attempts. Keep transport endpoints and the Claude executable in application configuration. The implementation uses the existing core `Provider` and `RoutingPolicy` interfaces, with the small runtime-driven changes below, rather than introducing a registry cache or provider factory.

The final user choices supersede the first PRD draft: signup requires **email, password, and a database-issued signup code**; login establishes a **management session cookie**; inference API tokens are issued separately after login and have independent metrics. Neither inference API tokens nor CIDR allowlisting authorize management. Email verification is outside this change. The user subsequently requested a static dashboard: complete, verify, and commit this API first, then build the dashboard in a separate commit against these endpoints.

Classification for this commit: backend HTTP service plus one PostgreSQL issuance script. Frontend guidance applies to the following dashboard phase, not this backend design. The static site will use same-origin HTTP calls and can be served by the existing application without SSR or CORS configuration.

## Runtime provider ownership

Create exactly one long-lived Codex backend and one Claude backend at application startup. The map is keyed by `ProviderKind`, not a configured instance ID. A selected database route supplies its immutable kind and instance ID:

```rust
enum ProviderKind { Codex, Claude }
enum RoutingStrategy { LeastUsed, Priority }

trait Provider {
    async fn infer(&self, provider_id: &str, request: InferenceRequest,
                   cancellation: CancellationToken) -> Result<EventStream, ProxyError>;
    async fn refresh_credentials(&self, provider_id: &str) -> Result<(), ProxyError>;
    async fn shutdown(&self);
}
```

Remove instance-ID fields from `CodexProvider` and `ClaudeProvider`; pass the ID to their existing credential/quota operations and capture an owned ID only when a background task requires it. Each backend retains its current shared `TaskTracker`. All requests, refreshes, and Claude control callbacks continue to use PostgreSQL credential locks. Gateway shutdown waits for request lifecycles and then both backend trackers.

Maintenance selects enabled provider IDs/kinds belonging to enabled accounts from PostgreSQL on each existing one-minute tick, then dispatches to the two backends. Newly registered providers are immediately usable for inference; maintenance sees them on the next tick. This needs no cache invalidation, poll loop on the inference path, or new backend object per request.

Remove `Config.providers` and `ProviderConfig`. Keep `claude_executable: PathBuf`, defaulting to the bundled absolute path, plus existing listen/concurrency/timeout settings. There is no account-controlled executable, upstream base URL, OAuth endpoint, client ID, or redirect URI.

## Relational schema

Add one forward SQLx schema change without rewriting applied migrations or deleting prior records.

| Relation | Additions / purpose |
| --- | --- |
| `accounts` | Normalized unique `email`, PHC `password_hash`, `routing_strategy` constrained to `least_used` / `priority`; existing label/enabled/usage totals remain |
| `signup_codes` | SHA-256 `code_hash` primary key with a 32-byte check, `created_at`; successful redemption deletes the row |
| `management_sessions` | UUID ID, account FK, unique SHA-256 token hash, created/expiry timestamps; no refresh-token mechanism |
| `provider_instances` | Immutable `kind`, `label`, `enabled`, `login_generation bigint NOT NULL DEFAULT 0`; retain text IDs and account ownership/history FKs |
| `provider_models` | Integer `priority`, default 0, in addition to existing model/provider/upstream mapping |
| `provider_login_sessions` | UUID ID, unique provider FK (one current attempt per provider), status, created/expiry timestamps; typed nullable Codex fields (`device_auth_id`, `user_code`, polling interval/next-poll time) and Claude fields (`state`, `code_verifier`) |
| `provider_quota` | `login_generation bigint NOT NULL DEFAULT 0`, fencing quota observations from an earlier upstream login |
| `requests` | Index `(api_token_id, started_at)` for management-side token metrics; existing request attribution is reused |

New account/provider writes always supply complete required settings. For existing data, infer provider kind only when exactly one corresponding credential table contains that provider. Do not guess the kind of credentialless or contradictory records. Keep unresolved kinds NULL and exclude them from runtime route/maintenance queries until the operator explicitly assigns the correct kind during cutover. Existing accounts lacking email/password cannot log into management until those fields are provisioned; their history and existing API-token records remain intact. These are explicit data-cutover requirements, not a fallback to static TOML registration. Normal management DTOs describe fully registered rows, and new registration cannot create an unresolved provider.

The SQL issuance script uses two concatenated `gen_random_uuid()` values for an unpredictable code, inserts `sha256(convert_to(code,'UTF8'))`, and returns the plaintext code once. Use a single CTE statement so issuance and returned output correspond. It requires only PostgreSQL/psql, introduces no general management CLI, and never writes plaintext codes to a table.

## Account authentication and HTTP boundary

Use Argon2id from `argon2` 0.6 with its maintained password-hash APIs and generated salts. Store PHC strings. Hash/verify in `spawn_blocking`, with a small semaphore permit moved **inside** the blocking task so cancellation does not release capacity before hashing stops. Preserve password bytes exactly. Normalize email with an explicitly documented trim/lowercase policy and apply bounded external-input validation; no email-verification workflow is added.

Session tokens and API tokens are independent random opaque secrets (two UUID v4 values are sufficient with the existing dependency). Store SHA-256 hashes, never plaintext. Management sessions expire after a fixed 24 hours, have no sliding writes, and logout deletes the matching session. Set an HttpOnly, SameSite=Strict, Path=/management cookie with the opaque session secret and matching Max-Age; return account metadata and expiry, not the secret, in JSON. The cookie is Secure by default. An explicit deployment-owned `cookie_secure=false` setting may support local HTTP development; never derive cookie security from an untrusted forwarded header. Issuing/revoking an API token does not alter the management session. Invalid email/password receives the same sanitized authentication response.

The management router uses Axum authenticated route middleware/extraction to load an enabled account from the session cookie through `management_sessions`, including expiry. Only signup and session creation are unauthenticated. Do not support a second bearer management-session mode. Require `X-LLMProxy-Management: 1` on every management mutation, including signup/login/logout; request bodies use application/json where present. Emit no CORS authorization headers. Cross-origin forms cannot supply the custom header, and cross-origin browser fetches requiring it fail preflight, without an external-origin setting. All resource SQL includes the authenticated account ID. A foreign ID receives the same 404 as a nonexistent ID. Unknown JSON fields, invalid enum values, excessive lengths and malformed external values receive 400; duplicates and incompatible current state receive 409; upstream failures are sanitized. Reuse the existing error response shape and add only necessary not-found/conflict error kinds.

Management request/response bodies must not enter existing inference body logging: they contain passwords, signup codes, login URLs/codes, and newly issued client tokens. Read responses never expose password hashes, token hashes, upstream credentials, OAuth verifiers, or refresh tokens. The static dashboard must not place management-session secrets in localStorage or JavaScript state; ordinary fetch calls use the HttpOnly same-origin cookie. Direct REST clients use a cookie jar and the same mutation header.

## HTTP contract

All endpoints below are under `/management`; resource IDs are server-generated. PATCH DTOs use only explicitly mutable fields and reject empty patches. GET collections return arrays; no generic query/filter framework is needed for this account-scoped first version.

| Method / path | Request / behavior |
| --- | --- |
| `POST /accounts` | `{email,password,label,signup_code}`; atomically redeem code and create account; return account record, **no inference API token** |
| `POST /sessions` | `{email,password}`; set session cookie and return `{expires_at,account}` |
| `DELETE /session` | Revoke current management session and expire its cookie |
| `GET /account` | Current account ID/email/label/settings only |
| `PATCH /account` | Update label; no self-disable, arbitrary identity reassignment, or password-reset system |
| `GET /api-tokens` | Own token IDs/labels/created/revoked state; no secret/hash |
| `POST /api-tokens` | `{label}`; return generated token once plus token metadata |
| `DELETE /api-tokens/{id}` | Mark own token revoked; retain history/FKs; repeated revocation is idempotent |
| `GET /api-tokens/{id}/metrics` | Own persisted request/outcome/usage/latency summary, including revoked tokens |
| `GET /providers` | Own fully registered provider IDs/kinds/labels/enabled/credential-presence metadata |
| `POST /providers` | `{kind,label,enabled?}`; create owned provider with generated text UUID; default enabled true |
| `GET /providers/{id}` | Owned provider metadata; credentials never returned |
| `PATCH /providers/{id}` | `{label?,enabled?}` only; kind and account ownership are immutable |
| `POST /providers/{id}/logins` | Start provider browser/device login; return opaque login ID, browser URL, expiry, and Codex user code/poll interval |
| `GET /providers/{id}/logins/{login_id}` | Safe state/scheduling only |
| `POST /providers/{id}/logins/{login_id}/complete` | Codex `{}` polls; Claude `{code}` submits `code#state`; pending/processing returns 202, completed returns safe status |
| `GET /routing` | `{strategy,routes:[{model,provider_id,upstream_model,priority}]}` |
| `PUT /routing` | Atomically replace own full routing configuration; empty route list is valid |

IP authentication remains operator-provisioned. The management API does not create or modify CIDRs: a logged-in account does not prove ownership of arbitrary network addresses. No IP schema or authentication changes are needed.

Signup transaction: validate/hash password before opening a transaction; `DELETE FROM signup_codes WHERE code_hash=$1 RETURNING ...`; insert account; commit. A duplicate email or other insertion failure rolls back code redemption. The script and HTTP code hash exactly the same plaintext bytes. Wrong/consumed codes create no rows, and concurrent redemption has at most one success.

## Routing read/write consistency

Extend `Route` with `provider_kind` and `priority`. Introduce a runtime routing context containing `AccountUsage` and `RoutingStrategy`. `PgStore::routing_snapshot(account_id, requested_model)` returns that context plus matching routes in **one SELECT** joining the account and enabled, configured provider/model rows. Keep a left join so an account with no matching route still returns its settings. Strategy and model mappings are therefore read from one PostgreSQL statement snapshot.

Fetch this snapshot and `provider_states(account_id)` concurrently. Remove configured-ID arrays and the now-redundant separate account-usage read from the gateway path. This reduces routing reads from three to two while preserving ownership filtering. Provider states/usage can change concurrently, as with existing quota/least-used selection, but strategy and route replacement cannot be mixed from different committed versions.

Retain the core-owned `RoutingPolicy` boundary and change its account input to the routing context. The configured implementation excludes fresh exhausted quota windows exactly as today, then selects deterministically:

- `least_used`: `(accumulated_tokens, provider_id, route_index)`.
- `priority`: `(priority, provider_id, route_index)`, where lower integer priority wins.

Do not silently add weighted routing, random selection, or inference retries. A disabled provider is excluded for new routing snapshots; already-admitted requests continue normally. The route replacement transaction locks the owning account row, verifies all referenced providers belong to it, validates duplicates, replaces only that account's mappings, updates strategy, and commits together. Rejection preserves the old configuration.

## OAuth orchestration and persistence

Use the protocol and exact fixed endpoints in [oauth-research.md](oauth-research.md). The production service does not run a login CLI or depend on Pod files.

- Codex start returns/persists device fields and UTC deadlines; split the existing poll helper so no `sleep_until` occurs while a database transaction is held. Too-early completion requests return pending immediately without contacting upstream.
- Claude start generates state/verifier, persists them, and returns the fixed manual-PKCE authorization URL. Complete validates the submitted `code#state` before claiming any operation. Optional token-response profile fields can populate existing metadata; no extra profile HTTP request gates saving credentials.

Use statuses `pending`, `processing`, `completed`, `failed` with a finite expiry and a unique provider reference, so only one current attempt exists per provider. Starting a login performs an account-scoped provider read and `INSERT ... ON CONFLICT(provider_id)` that locks/rechecks only the login-session row. It may replace pending/failed/expired state, but returns 409 for an unexpired processing operation. Do not explicitly lock the provider row during start: completion holds the session row before updating the provider's generation, and reversing that order would deadlock. Immutable ownership/kind and the lack of provider deletion make the scoped read plus foreign key sufficient.

A completion request atomically changes the expected pending session to processing and commits **before** making an upstream call. Only the claimant may proceed. Launch the operation in a shared login `TaskTracker`, so dropping the HTTP future does not abandon token persistence. For Codex pending responses, update scheduling and return the same session to pending; other calls observe processing and do not poll again. If a consuming exchange fails ambiguously, mark failed and require a new login rather than repeating the authorization code. A process killed during an in-flight operation can leave it processing until expiry; pending sessions still survive ordinary restarts, and graceful shutdown drains active operations. This conservative behavior avoids a lease/retry subsystem for potentially consumed codes.

After the durable claim, the worker opens a transaction and locks/rechecks the current session row by both session ID and provider ID. If replacement won before this lock, the worker performs no upstream call. Hold this narrow session lock across the bounded actual HTTP operation and persistence, so expiry/replacement/cleanup cannot invalidate a successful exchange while it is being saved. A late replacement waits, then rereads the completed state. This does not lock an account or provider usage-counter row during network I/O.

For an existing credential row, next acquire its existing refresh lock before token exchange, so login and refresh cannot submit/commit stale credential changes out of order. The order is session row, credential row, then the provider generation update only after HTTP has completed. Fresh inference reads use MVCC and do not require these locks. Initial login has no credential row and is serialized by the session lock. Save/upsert credentials, increment `provider_instances.login_generation`, and mark the claimed session completed in the same transaction. Clear verifier/device secret fields on terminal completion/failure. Starting a new pending login must not delete existing working credentials.

Fresh credential reads join the provider row and return the captured login generation in the same query. Locked credential paths first acquire/read the credential row, then read its provider generation in a second statement while retaining the lock. PostgreSQL's wait/recheck behavior can otherwise combine a newly committed credential with an old joined generation; the reviewer reproduced that on PostgreSQL 17.11. Login always locks existing credentials before changing generation, so the second read is stable. Ordinary token refresh retains that generation. A successful new login increments it exactly once. Quota writes carry the captured generation; UPSERT accepts only a higher generation or a non-older observation within the same generation. Routing joins quota only when its generation matches the current provider generation. Therefore a late old request cannot apply its upstream account's limits to a new login, even after the new login has emitted quota. Claude control-protocol refresh callbacks also supply their original expected generation and fail if it changed; they must never inject a newly logged-in account's token into an existing CLI request. No additional fresh-credential inference query or quota-write row lock is introduced; only an already-locking refresh path gains the generation read.

Reuse the existing maintenance tick to delete expired management sessions and expired provider-login rows. OAuth cleanup uses row locking with `SKIP LOCKED`, so a worker actively exchanging/saving cannot lose its row. A dead process releases the lock and its finite-expiry attempt becomes eligible for cleanup or replacement. There is no separate cleanup service.

The API reports completion only after commit. No design can make an external non-idempotent exchange and PostgreSQL atomic across a hard process kill; recovery from that interval requires a new login. This limitation must be stated rather than hidden behind blind retry.

## Per-token metrics

Use existing `requests.api_token_id` for durable attribution. The management metrics endpoint first scopes the API token to the logged-in account, then aggregates that token's request rows in one query: total/running/succeeded/failed/cancelled/timed-out/interrupted counts, reported input/output token totals, missing-usage request count, mean finalized latency, and last request time. Return null latency/last time when there are no samples. The new index supports these off-path reads. No request-history aggregation is added to routing and no new per-request counter table is necessary.

Prometheus admitted-lifecycle counters, provider errors, active requests, token usage, and request-latency histograms gain safe `api_token_id` and `auth_source` labels. Token IDs are UUIDs; CIDR requests use an empty token label and `auth_source="ip"`. Never label with token secrets, email, or management-session IDs. Keep unidentified authentication-stage metrics aggregate.

Gateway lifecycle finalization remains the single owner of admitted-request metrics. Add a separate rejected-request counter for authenticated requests rejected before durable admission (validation, routing, quota, concurrency, or begin-request failure), labeled by token ID/auth source/error kind. API handlers count only errors returned before a `GatewayStream` exists; stream failures are finalized in the gateway. Unauthenticated failures have no trustworthy token identity. Persisted token summaries describe admitted database request records; the rejection counter is process-local Prometheus data and is not falsely included in durable history. Appended token labels intentionally increase metric cardinality and require measurement of hot-path overhead.

## Module and implementation contracts

- `src/core.rs`: `ProviderKind`, `RoutingStrategy`, routing context, extended `Route` and ID-aware `Provider` trait.
- `src/store.rs` / a cohesive `src/store/management.rs` submodule: typed settings/sessions/management queries; keep `PgStore` as the existing persistence abstraction. Do not export the raw pool solely for tests.
- `src/management.rs` or `src/management/{mod,auth,oauth}.rs`: HTTP DTOs/routes, account-session authentication/password work, OAuth operation orchestration. Split only where responsibility and size warrant it.
- `src/providers/{codex,claude,mod}.rs`: protocol methods, shared per-kind backends, DB-driven maintenance.
- `src/gateway.rs`, `src/metrics.rs`, `src/api.rs`: snapshot routing, token attribution and pre-admission rejection ownership.
- `scripts/issue-signup-code.sql`: PostgreSQL issuance statement.
- `tests/management_api.rs`: end-to-end management HTTP/ownership/session/signup/OAuth/routing/token-summary behavior using isolated schemas and fake upstreams.
- Existing provider/gateway/API/config/Helm tests are updated for the actual runtime contract, with no compatibility constructors or test-only production derives.

## Implementation order and reviews

| Order | Cohesive block | Critical | Dependencies / owner boundary |
| --- | --- | --- | --- |
| 1 | Typed persistence, signup/email/session auth, token HTTP operations | Yes | Foundation; one owner edits schema/store and auth API together |
| 2 | DB provider registration, two-backend runtime and coherent routing | Yes | Schema contracts from 1; owns core/gateway/runtime/settings and connected tests |
| 3 | Durable Codex/Claude login orchestration | Yes | Provider/account/session contracts from 1–2; owns OAuth/protocol code and fixtures |
| 4 | Token metrics, deployment examples and full workflow verification | Yes | Existing attribution plus 1–3; may prepare docs/tests once interfaces are fixed |

Review the schema/auth ownership boundary before dependent implementation. Runtime and OAuth changes share provider files: do not let two implementers edit them concurrently without explicit handoff. Root coordinates complete integration and performance measurements. No production changes are made by this architecture task.

## Validation and measured limits

Integration coverage must prove: atomic signup-code redemption with duplicate email rollback and concurrent reuse; password authentication and expiry/logout; cookie flags and mutation-header/CSRF rejection; API-token/CIDR inability to manage; ownership isolation for all resource families; two independent app instances observing registration/configuration and persisted login state; priority/least-used/quota behavior; atomic rejected routing replacement; multiple API tokens with separate durable and Prometheus statistics; safe unauthenticated/rejected/admitted metric boundaries; OAuth wrong-state/expiry/replay/concurrent completion; cancellation after exchange with durable save; refresh/login ordering and shutdown drain; blocked near-expiry exchange versus replacement/cleanup; generation-fenced old-request quota and Claude callbacks after relogin; credentials generated by a fake login working with the official Claude CLI against a fake Messages API.

Run the complete relevant existing suite, fmt, locked Clippy with warnings denied, and Helm rendering checks. Reuse the unchanged release API benchmark to compare before/after in alternating order, including short and large streaming outputs, both APIs, concurrency 1/4, and 100,000 historical requests. Record query counts and total/first-text p50/p95; report regressions and shared-host variability. The benchmark includes local DB/logging and excludes real subscription latency/network and deployed collectors. No real account login, email delivery, or Kubernetes deployment is claimed.
