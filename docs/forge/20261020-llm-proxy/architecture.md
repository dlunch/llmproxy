# Rust LLM Proxy Architecture

## 1. Overview

Build one Rust service with separate API, gateway, and provider modules. Registered Rust trait implementations are the plugin mechanism. HTTP models and provider wire formats remain at their respective boundaries. PostgreSQL is authoritative for authentication, credentials, and request accounting. No management CLI, management HTTP endpoints, or dashboard is included; operators provision test data using SQL.

The repository has no existing implementation to preserve. Protocol support covers text generation, native web search, and client-defined function call/result roundtrips, both streaming and non-streaming, through Chat Completions and Responses. Unsupported image, audio, persistence, host tool execution, and structured-output features return explicit validation errors.

## 2. Structure and ownership

```text
HTTP client -> api (token authentication, protocol conversion)
                 -> gateway (routing, lifecycle, cancellation, accounting)
                      -> Provider trait -> Codex HTTP / Claude subprocess
                      -> PgStore -> PostgreSQL
                 -> Metrics -> Prometheus scrape
main -> configuration, provider registration, application composition
```

- `core`: consumer-owned provider contracts, request/event types, error categories; no axum, SQLx, or concrete provider imports.
- `gateway`: provider registry, replaceable routing policy, orchestration, deadlines and lifecycle persistence.
- `store`: SQLx PostgreSQL operations, migrations, typed credential/snapshot records.
- `api`: authenticated API handlers and protocol-specific serialization.
- `providers/codex`: direct Codex OAuth and Responses transport; never invokes Codex CLI.
- `providers/claude`: controlled official Claude Code executable in print mode.
- `metrics`: bounded-label counters, histograms, gauges using `prometheus-client`.
- `config`, `main`: read configuration and wire concrete implementations.

## 3. Shared interfaces

The following contracts summarize the implemented boundaries. Concrete field definitions remain in the source files.

```rust
// core.rs
pub struct Principal { pub account_id: Uuid, pub credential: Credential }
pub enum Credential { ApiToken(Uuid), AllowedIp { range_id: Uuid, address: std::net::IpAddr } }
pub struct AccountUsage { pub account_id: Uuid, pub input_tokens: u64, pub output_tokens: u64 }
pub struct Message {
    pub role: Role, pub content: String,
    pub tool_calls: Vec<ToolCall>, pub tool_call_id: Option<String>,
}
pub enum Role { System, Developer, User, Assistant, Tool }
pub struct FunctionDefinition {
    pub name: String, pub description: Option<String>,
    pub parameters: serde_json::Value, pub strict: Option<bool>,
}
pub enum Tool { Function(FunctionDefinition), WebSearch }
pub enum ToolChoice { Auto, None, Required, Function(String) }
pub struct ToolCall { pub id: String, pub name: String, pub arguments: String }
pub enum WebSearchStatus { InProgress, Searching, Completed }
pub struct InferenceRequest {
    pub model: String, // client alias; replaced with routed upstream model for Provider::infer
    pub messages: Vec<Message>,
    pub max_output_tokens: Option<u32>,
    pub temperature: Option<f64>,
    pub top_p: Option<f64>,
    pub stop: Vec<String>,
    pub tools: Vec<Tool>,
    pub tool_choice: ToolChoice,
}
pub struct Usage { pub input_tokens: u64, pub output_tokens: u64 }
pub enum FinishReason { Stop, Length, ToolCalls }
pub enum ProviderEvent {
    TextDelta(String),
    ToolCallDelta { index: usize, id: Option<String>, name: Option<String>, arguments: String },
    WebSearch { id: String, query: Option<String>, status: WebSearchStatus },
    Citation { url: String, title: String, start_index: usize, end_index: usize },
    Usage(Usage), // latest cumulative snapshot, never an additive increment
    Finished(FinishReason),
}
pub type EventStream = Pin<Box<dyn Stream<Item = Result<ProviderEvent, ProxyError>> + Send>>;
#[async_trait]
pub trait Provider: Send + Sync {
    async fn infer(&self, request: InferenceRequest,
                   cancellation: CancellationToken) -> Result<EventStream, ProxyError>;
    async fn shutdown(&self) {}
}
pub enum ErrorKind {
    InvalidRequest, Unauthorized, ModelNotFound, QuotaExceeded,
    Unavailable, UpstreamAuth, Upstream, Timeout, Cancelled, Persistence,
}
pub struct ProxyError { pub kind: ErrorKind, pub message: String }
```

Only safe messages reach clients. Provider response bodies, stderr, credentials, and raw transport errors must not become `message`. Use `async-trait`, `futures`, and `tokio-util` for object-safe asynchronous providers and cancellation.

```rust
// gateway.rs
pub struct Route {
    pub model: String, pub provider_id: String, pub upstream_model: String,
}
pub struct ProviderState {
    pub provider_id: String,
    pub accumulated_tokens: u64,
    pub quota: Vec<QuotaWindow>,
}
pub struct QuotaWindow {
    pub key: String,
    pub observed_at: DateTime<Utc>,
    pub remaining_fraction: f64,
    pub resets_at: Option<DateTime<Utc>>,
}
pub trait RoutingPolicy: Send + Sync {
    fn select(&self, request: &InferenceRequest, account: &AccountUsage, routes: &[Route], states: &[ProviderState])
        -> Result<usize, ProxyError>; // index into routes
}
pub struct GatewayStream { /* receiver + cancellation token + request metadata */ }
// Implements Stream<Item = Result<ProviderEvent, ProxyError>>.
// Drop cancels its token; persistence task owns the provider stream.
impl Gateway {
    pub async fn execute(&self, principal: Principal, request: InferenceRequest)
        -> Result<GatewayStream, ProxyError>;
}
```

`GatewayStream` exposes `request_id: Uuid` and `created_at: DateTime<Utc>` for API identifiers and timestamps. Gateway stores providers as `HashMap<String, Arc<dyn Provider>>`. Each configured ID is a provider instance owned by one account in PostgreSQL; multiple instances of the same provider kind are allowed. Gateway filters eligible routes by account ownership before invoking any routing policy, so replacing a policy cannot bypass authorization. API composition receives `Arc<Gateway>`, `PgStore` and shared metrics. Gateway receives an authenticated Principal and persists the account ID plus either token identity or matched IP range and peer IP. SQLx uses ipnetwork conversion only at the PostgreSQL boundary. Providers receive credentials/configuration at construction and do not import gateway or API modules.

`PgStore` wraps `PgPool` and exposes `authenticate_token(digest)` and `authenticate_ip(address)` returning `Option<Principal>`, `account_usage(account_id)`, `begin_request(...)`, `finish_request(...)`, `provider_routes(account_id, registered_ids)`, `provider_states(account_id, registered_ids)`, `lock_codex_credentials(provider_id)`, `lock_claude_credentials(provider_id)`, and transaction-consuming credential save methods, `save_quota(...)`, and `reconcile_expired_requests()`. Use typed parameter/result structs where a method would otherwise have many arguments. There is no repository trait until a real alternative storage implementation requires one; gateway integration tests use PostgreSQL.

Codex authentication is a reusable Rust API on `CodexAuth`: asynchronous `start_device_login`, `poll_device_login`, and `refresh`. Polling returns a typed pending/success/expired result and respects upstream interval. Successful credentials include access token, refresh token, account identifier and expiry when available. Verified Codex source commit `091008729d5572712ac08964a9602eb71cf31428` uses client ID `app_EMoamEEZ73f0CkXaXp7hrann`: POST `https://auth.openai.com/api/accounts/deviceauth/usercode` with JSON client_id; direct the user to `https://auth.openai.com/codex/device`; poll `/api/accounts/deviceauth/token` with device_auth_id/user_code, treating 403/404 as pending for up to 15 minutes. Exchange authorization_code/code_verifier at `/oauth/token` with form grant_type authorization_code and redirect_uri `https://auth.openai.com/deviceauth/callback`. Refresh uses JSON grant_type refresh_token/client_id/refresh_token at the same endpoint. Extract account ID from the id_token claim `https://api.openai.com/auth.chatgpt_account_id`. No local callback server is required. Credentials can be provisioned through SQL; authentication APIs do not require an operator-facing command or HTTP endpoint.

## 4. Data and lifecycle

1. API validates a supplied bearer token against its SHA-256 digest in PostgreSQL, checking revocation; database errors fail closed. A supplied invalid token cannot fall back to network access. When no token is supplied, the direct TCP peer IP may authenticate through account_ip_ranges CIDRs in PostgreSQL; forwarded headers never influence identity. Both authentication paths require an enabled account. Overlapping ranges select the longest matching prefix; exact duplicate networks are disallowed. Token lookup never logs raw bearer values.
2. API validates the supported protocol and converts text inputs into `InferenceRequest`. Gateway reads routing state for registered provider instances owned by the authenticated account and account-level accumulated usage. It loads model routes from PostgreSQL for those instances on each request before passing the normalized request, account usage, filtered routes and state to the routing policy.
3. Gateway acquires the bounded concurrency permit and inserts a running request with a fixed deadline before calling the provider. Failed insertion prevents inference.
4. Gateway creates a bounded channel and starts one task owning the provider stream, cancellation token, permit and accounting state. The returned `GatewayStream` cancels this token when dropped.
5. The task owns Provider::infer setup as well as the returned stream and selects both phases against cancellation and deadline. Sending to a full output channel must also select cancellation/deadline, so stalled clients cannot retain work indefinitely. Text is forwarded; usage replaces the latest cumulative usage snapshot.
6. A valid provider terminal event triggers conditional final persistence before forwarding `Finished`. EOF without a terminal event is an upstream failure. On upstream failure/cancellation/timeout, finalize the row once with any observed usage. Dropping a provider stream must terminate its request/process.
7. Persistence failure emits a safe persistence error rather than success. Already streamed text cannot be recalled. Leave the request running for reconciliation if finalization could not be saved; do not fabricate usage.
8. On startup and periodically, mark running rows whose deadlines expired beyond the reconciliation grace period as interrupted. Updates use `WHERE outcome = 'running'` so competing finalizers cannot overwrite a terminal row. Reconciliation cannot recover unreported upstream usage.

Default routing filters exact model aliases and exhausted fresh quota; among eligible routes, chooses the smallest accumulated token total, then provider ID for deterministic ties. Unknown or stale quota remains eligible but is never represented as unlimited or zero. Snapshot freshness is a documented fixed policy duration, initially five minutes; a passed reset time makes exhaustion obsolete. Local cumulative usage comes from summing nullable persisted request token counts for the provider, including partial usage on failed calls. Missing counts do not become reported zero usage. No duplicated total counter exists, so concurrent completions cannot lose increments. Usage selection is advisory, not a strict quota reservation.

## 5. PostgreSQL schema

Use SQL migrations and typed columns:

- `accounts(id uuid primary key, label text not null, enabled boolean not null default true, created_at timestamptz not null default now())`.
- `account_ip_ranges(id uuid primary key, account_id uuid not null references accounts(id), network cidr unique not null, created_at timestamptz not null default now())`.
- `api_tokens(id uuid primary key, account_id uuid not null references accounts(id), label text not null, token_hash bytea unique not null, created_at timestamptz not null default now(), revoked_at timestamptz null)`; require a 32-byte hash.
- `provider_instances(id text primary key, account_id uuid not null references accounts(id), unique(account_id,id))` binds configured runtime provider IDs to account owners. Runtime kind and connection settings remain in TOML.
- `provider_models(provider_id text not null references provider_instances(id), model text not null, upstream_model text not null, primary key(provider_id,model))` defines available client aliases and upstream names. Both names must be nonempty. Model changes take effect on subsequent requests without restart.
- `provider_credentials(provider_id text primary key references provider_instances(id), access_token text not null, refresh_token text not null, account_id text not null, expires_at timestamptz null, updated_at timestamptz not null default now())` for Codex. Database access is trusted operator access; credentials must never appear in ordinary queries or diagnostics. Claude subscription credentials and required login metadata live in `claude_credentials`, keyed by provider instance; invocation-private directories are hydrated from the database and refreshed authentication is persisted before terminal completion.
- `requests(id uuid primary key, account_id uuid not null references accounts(id), ip_range_id uuid null references account_ip_ranges(id), api_token_id uuid null references api_tokens(id), client_ip inet null, requested_model text not null, provider_id text not null, upstream_model text not null, started_at timestamptz not null, deadline_at timestamptz not null, finished_at timestamptz null, outcome text not null, error_kind text null, input_tokens bigint null, output_tokens bigint null)`; require either api_token_id alone or both ip_range_id/client_ip as the credential source and constrain outcomes to running/succeeded/failed/cancelled/timed_out/interrupted and counts to nonnegative values. A composite foreign key `(account_id,provider_id)` references `provider_instances(account_id,id)`, enforcing provider ownership in persisted history. Index account ID, provider ID, and the running deadline lookup.
- `provider_quota(provider_id text not null references provider_instances(id), window_key text not null, observed_at timestamptz not null, remaining_fraction double precision not null, resets_at timestamptz null, primary key(provider_id, window_key))`; constrain fraction to `[0,1]`. Retain each independently resetting quota window; any fresh exhausted window excludes a provider. Missing telemetry has no row.
- `provider_statistics` is a SQL view joining provider instances with requests and grouping by account and instance ID. It exposes request and per-outcome counts, nullable summed input/output tokens, requests missing usage, mean latency in milliseconds, and last request time. Instances without requests remain visible; unknown usage remains distinguishable from reported zero.

All client credentials and account IP ranges live in PostgreSQL. Provider model routes live in relational `provider_models` rows. Provider process/HTTP settings live in typed TOML config. Requests contain no prompt, completion, raw token, or provider error body. Record latency from request timestamps and metrics. Manual SQL examples use PostgreSQL `sha256(convert_to('test-token', 'UTF8'))`; no token management CLI or token-generation production API is added.

## 6. External API and provider behavior

- `POST /v1/chat/completions`: model, text messages, stream; expose supported sampling/length fields only when the selected provider can honor them. Non-stream response has one assistant choice with text, tool calls and citations as applicable, plus usage if known. SSE uses chat completion chunks, a finish chunk, optional requested usage chunk, and `[DONE]`.
- `POST /v1/responses`: model, string input or supported text message items, optional instructions, stream; support text, function-call/function-result items and requested native search. Non-stream responses contain the corresponding assistant message, function calls, and search outputs. SSE emits ordered Responses lifecycle/content events with stable response/item IDs and monotonically increasing sequence numbers, ending with `response.completed` or `response.failed`; length termination uses `response.incomplete` and incomplete_details reason max_output_tokens. Set upstream persistence off; reject local response retrieval, previous-response chaining, unsupported `store:true`, unsupported tool types and multimodal content.
- `/health` reports process liveness; `/metrics` exposes Prometheus text format. Bind address and network controls define operational endpoint exposure.
- Unknown or unsupported request fields that affect semantics are rejected instead of silently ignored; validate roles, text forms, ranges, unsupported `n`, and output limits before inference. Unsupported provider options fail before starting the upstream call.

Codex uses direct OAuth credentials and `POST https://chatgpt.com/backend-api/codex/responses` with Bearer authorization and `ChatGPT-Account-ID`, streaming upstream even when clients request a collected result. Refresh expired credentials under a PostgreSQL credential row lock; persist replacement refresh tokens before using updated state. HTTP streams are parsed incrementally with bounded frames and must end with a recognized terminal response event. Extract usage and trustworthy quota headers/events when present. No automatic inference retry after sending a request.

Claude uses the official Claude Code executable with `-p`, stream-JSON input and output, and incremental text events. Normal user input is sent on stdin after SDK initialization. Disable local host tools and permission prompts, allow only requested native web search and explicit client function bridge tools, isolate working/config directories, and avoid loading project settings or server repository files. Do not use `--bare`, which changes subscription authentication. Verified CLI 2.1.302 supports --resume with an absolute JSONL history path, preserving user/assistant turns, plus --system-prompt. Isolation uses --safe-mode, an explicit --tools list, --strict-mcp-config, a controlled MCP configuration, --setting-sources with an empty value, --permission-mode dontAsk, --permission-prompts none, and --no-session-persistence. Use the controlled SDK MCP transport to advertise client function definitions and return tool-call requests to clients; never execute those functions on the proxy host. Resume JSONL histories preserve tool_use/tool_result blocks for roundtrips. A continuation ending in client tool results writes the complete final user result row into that history and sets `CLAUDE_CODE_RESUME_INTERRUPTED_TURN=1`, allowing the native CLI to continue it without another stdin user turn. This avoids the CLI repairing unfinished tool uses before stdin results arrive. Tool results use text-block arrays so native environment reminders remain separate blocks. Function handoff waits for `message_stop`, retaining every parallel call and the final usage delta; cumulative usage sums distinct upstream message IDs, including native search turns, while updates replace the same message’s snapshot. Pending client `tools/call` requests never execute. Ordinary completion still requires a successful CLI result and exit; client-function handoff terminates the process after the completed assistant message. Spawn with kill-on-drop and explicitly kill/wait on cancellation. Bound stdout records, consume bounded stderr without exposing it, require a successful terminal JSON result and successful process exit for ordinary completion. Process exit or EOF alone is not successful inference.

## 7. Errors and observability

API maps invalid request to 400, invalid/revoked token to 401, unsupported model to 404, known exhausted quota to 429, unavailable provider to 503, upstream/auth-provider failure to 502, deadline to 504, and persistence failure to 503. Midstream errors use the selected protocol's error representation and never emit a success terminal afterward. Client authentication and upstream authentication remain distinct categories.

Metrics include request outcome count, latency histogram, active request gauge, provider error count, and observed input/output token counters. Labels use configured provider IDs, endpoint names, outcomes and error categories only. No arbitrary requested models, client identifiers, token strings or error messages become labels. Accounting task ownership releases permits and decrements active gauge on every exit.

## 8. Implementation plan and files

```text
Cargo.toml
src/{lib,main,config,core,gateway,store,metrics,api}.rs
src/providers/{mod,codex,claude}.rs
migrations/0001_initial.sql
config.example.toml
tests/{api_integration,gateway_integration,codex_provider,claude_provider}.rs
README.md
```

| Order | Work unit | Core review | Dependencies | Parallel work |
|---|---|---|---|---|
| 1 | Core contracts, store, gateway, metrics | Yes | None | Establish shared types first |
| 2 | Codex and Claude providers | Yes | Core contracts | Alongside API after contracts exist |
| 3 | API adapters and composition | Yes | Core contracts, gateway/store interfaces | Alongside providers |
| 4 | End-to-end setup and verification | Yes | All | Integrate and review together |

Use axum, tokio, reqwest with rustls, SQLx/PostgreSQL, serde, serde_json, toml, uuid, chrono, sha2, async-trait, futures, tokio-util, prometheus-client, tracing, and thiserror only where they remove substantive implementation work. Stream parser dependencies may be used after checking their framing and cancellation support. Avoid separate crates, dynamic loading, storage abstraction wrappers, or duplicated accounting caches.

## 9. Verification

Prefer observable integration coverage, reusing common fixtures:

- PostgreSQL migration and SQL token seed; both endpoints authenticated and revoked/invalid tokens rejected before provider execution.
- Fake provider through real gateway/API: text, streaming and collected responses; usage and request outcome in PostgreSQL; Prometheus counters and gauge balance.
- Routing: model filtering, deterministic least-usage choice, selection changes after completion, exhausted/fresh quota, stale and unknown quota.
- Concurrent calls retain all usage; cancellation/deadline stop provider work and finalize rows; provider failure and truncated streams never report success.
- Persistence failures before execution prevent invocation; final persistence failure does not return success; expired-row reconciliation preserves terminal rows.
- Local HTTP fixtures verify Codex authorization headers, device polling, refresh persistence, chunked SSE, malformed/failed terminal events and usage extraction.
- Controlled subprocess fixtures verify Claude invocation, streamed records, usage, nonzero exit, bounded output and process termination on cancellation. Inspect real executable help/source to ensure subscription-safe isolation options exist.
- Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`; run database tests with an explicit test database. Clearly distinguish skipped account-dependent tests from passed fixture tests.

## 10. Assumptions and limits

The subsequent container deployment supports multiple replicas using PostgreSQL row-lock transactions for shared credentials. Reconciliation uses persisted request deadlines and is safe across process restarts. Known quota figures depend on actual provider telemetry, and unknown upstream usage cannot be recovered after abrupt termination. Strict quota reservations, billing accuracy, administrative UI, host tool execution, and stored Responses are outside this initial text proxy scope.

Project classification: backend. Supplemental frontend guidance: not applicable. Claude function bridge behavior is covered by subprocess fixtures and an opt-in installed-CLI test against a local fake Messages API. That test covers parallel function handoff and result continuation without subscription access; real subscription behavior remains untested.

## Tool and search protocol handoff

Both APIs accept client function definitions as externally defined JSON schemas. Tool choice supports auto, none, required, or a named function; providers must honor the requested choice or explicitly reject unsupported constraints. Assistant tool calls and client tool results retain stable call IDs. The normalized event stream carries incremental arguments by call index, native web-search status, and URL citations. Gateway bounds total text, arguments, identifiers, queries, and citation string bytes to 8 MiB. Function-only output produces function output items without inventing empty text.

Codex sends native function and web_search tools, maps assistant function_call and client function_call_output input items, and parses native call/search/citation events. Claude exposes client functions through its controlled SDK MCP transport, intercepts calls for return to the API client, and consumes client-supplied results on the next request. Only the explicitly requested native web search tool may execute server-side; no generic host command or filesystem tool is enabled.

Add integration fixtures covering definitions/tool choice, fragmented arguments, function-only responses, tool-result history, web search statuses/citations, streaming and collected responses in both protocols, and provider cancellation while a function is pending.

Allowed-IP integration coverage must exercise both matched IPv4/IPv6 CIDRs and denied peers, verify spoofed forwarded headers do not grant access, reject invalid supplied bearer tokens even on allowed networks, and persist the peer IP without a token ID.

Account history uses `SELECT * FROM requests WHERE account_id = $1 ORDER BY started_at DESC`; usage uses sums of input_tokens/output_tokens grouped by account_id across every owned token and allowed network. No management or history HTTP endpoint is added. Tests verify shared usage across tokens and IP credentials, longest-prefix matching, disabled accounts, and isolation between account totals.

## Operational logging and lifecycle refinements

The user confirmed PostgreSQL history contains only account-level metadata and usage. Request/response bodies and operational events are emitted as JSON on stdout, correlated by request/account IDs. Streaming events are logged individually; `body_json` is a serialized JSON field. Authentication headers and provider secrets are excluded. No separate audit store is implemented.

Gateway workers are tracked until shutdown. After the HTTP server drains, gateway shutdown waits for accounting tasks and each provider lifecycle hook. Codex's hook waits for its bounded credential refresh task to persist rotation. The credential row lock is acquired before detaching the credential transaction, so cancelled queued requests do not create unbounded detached tasks.
