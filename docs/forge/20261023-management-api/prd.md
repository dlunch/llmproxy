# Account management API

## Purpose and context

Make a Kubernetes deployment operable through HTTP: create an account, register its subscription providers, complete browser authentication, configure model routing, and perform inference without manually inserting credential rows or restarting Pods. The existing Rust/Axum/PostgreSQL service exposes only inference and operational endpoints. Codex device-login helpers exist but have no HTTP caller; Claude initial login currently needs manual provisioning.

## User decisions

- Management is account-scoped. The user refined the initial bearer-token choice: signup uses email/password, management uses a login session, and inference API tokens are issued separately from the logged-in account's management interface. There is no global administrator token or role system.
- Account creation requires a signup code stored in PostgreSQL and issued by a script. This supersedes the earlier public-signup answer.
- Email verification is explicitly deferred. An account may own multiple API tokens, and metrics must track each token independently.
- Delivery is sequential: finish and commit the management API, then build a static account dashboard in a separate commit. The dashboard consumes this API and needs no server-side rendering.
- Model/provider mappings and selectable least-usage or priority routing are required.
- Earlier context explicitly includes initial Codex and Claude browser login and durable credential storage in the management API.
- Keep the reusable core separate from HTTP and host integrations. Use typed relational columns, existing dependencies, and the smallest cohesive implementation. Avoid backward-compatibility paths and speculative features.
- Request latency remains a primary constraint. Preserve authentication, ownership, quota windows, token rotation, cancellation, and durable accounting.

## Scope

### Required behavior

1. Issue unpredictable signup codes through a repository script using the existing PostgreSQL deployment. Store only a hash. By stated default, each code is single use and is consumed atomically with successful account creation; failed creation does not consume it.
2. Create an account using email, password, and signup code. Normalize and uniquely identify the email; hash passwords using an established password-hashing library. Provide login and logout with finite-lived, opaque, hashed management sessions. Do not issue inference API tokens during signup or login. Email verification is not part of this change. Password hashing/verification must not block async executor threads.
3. Read and update the current account's label/settings, list/issue/revoke multiple API tokens. Return newly issued API token plaintext once and store only hashes. Subsequent management operations require the account's active login session; inference API tokens and IP allowlisting do not grant management access. Preserve disabled-account and revoked-token semantics.
4. Register/list/read/configure account-owned Codex and Claude provider instances. Configuration includes a user-facing label and enablement; host executable paths and upstream transport URLs remain server-owned. Resource IDs are server-generated. Provider kind and ownership cannot be silently changed beneath existing credentials or requests.
5. Provider registration and configuration must affect new inference requests across replicas without a Pod restart. Already-running requests retain their current execution context. Keep provider request history intact.
6. Start, inspect, and complete Codex device-code login through account-authenticated HTTP operations. Save successful credentials in the provider row before reporting success. Do not invoke Codex CLI.
7. Start, inspect, and complete Claude browser login through HTTP operations with a user-submitted authorization code when necessary. Use the existing official authentication protocol, preserve required account metadata, and save credentials before reporting success. Users must not need Pod-local credential extraction.
8. Login sessions must have finite lifetimes and work across replicas; normal polling/completion must not require sticky routing. Bind sessions to the owning account and provider, enforce one-time completion, and keep state/verifiers/tokens out of logs and ordinary read responses. Do not automatically repeat a potentially consumed authorization-code exchange.
9. Read and atomically update account routing configuration: public model names, provider mappings, upstream model names, priorities, and account strategy (`least_used` or `priority`). Reject foreign providers and malformed/duplicate mappings. Both policies preserve current exhausted-quota exclusion and deterministic selection. No automatic replay of inference on a different provider.
10. Document and test a complete HTTP workflow from issued signup code through account/provider/login/routing to authenticated inference. Update configuration, Helm examples, and runtime initialization for DB-owned provider registration.
11. Track inference per API token using its non-secret ID: request outcomes, usage, latency, and errors in Prometheus, with account-scoped durable per-token statistics available to management clients. Existing request records already reference API token IDs; reuse them and keep aggregate statistics reads off the inference path. Distinguish CIDR-authenticated traffic and document counting boundaries. Never expose another account's token statistics through management endpoints.

### Scope boundaries

The dashboard is a separately requested next phase after the API commit; its concrete browser use should inform session security now. Existing operator-managed IP authentication remains unchanged; self-service CIDR registration is outside scope because unverified ranges cannot safely establish account identity. No general management CLI, email verification, external identity provider, billing, RBAC, provider-specific arbitrary command/URL settings, speculative routing policies, or destructive account/history deletion. The explicitly requested signup-code script is included. No live subscription operation or cluster deployment is required for implementation validation.

## Architecture constraints and data

PostgreSQL remains the source of truth for accounts, hashed passwords, management sessions, hashed client tokens, CIDRs, provider ownership, typed provider settings, model routes, quota and usage, signup codes, and transient OAuth login sessions. Keep externally defined Claude credential/profile documents as JSON where already justified. Configuration files keep deployment-owned settings only. Remove obsolete static provider-registration paths and update all callers/tests/examples together. Schema changes must not silently discard request history or credentials; document any explicit setup/cutover requirement rather than pretending obsolete configuration still works.

Use existing `Provider`, `RoutingPolicy`, `PgStore`, request accounting, and refresh coordination where applicable. Dynamic providers must not add an extra sequential database round trip to every inference request. Compare the existing representative release benchmark before and after request-path changes; report mixed results honestly.

## Failure behavior

- Invalid or consumed signup code creates no account. Duplicate email and other failed signup attempts do not consume a code. Concurrent redemption has at most one success.
- Missing/invalid/expired/revoked management session or disabled account cannot manage resources. Inference API tokens cannot manage accounts. A valid management session cannot read, modify, log in, or route through another account's provider/session/token.
- Failed route replacement leaves the previous configuration intact.
- Expired/cancelled/consumed login sessions cannot exchange again. OAuth errors are sanitized; upstream credentials never appear in errors/logs or provider-list responses.
- Requests cancelled after a successful token exchange must not discard rotated/new credentials. Concurrent refresh/login completion must not overwrite a newer login with stale credentials.
- Database changes that disable a provider affect later selection while existing requests complete and account normally.

## Validation

Prefer integration tests over implementation-mirroring unit tests. Reuse existing database/HTTP/official-CLI fixtures. Required coverage includes atomic signup-code redemption; email uniqueness and password login/session expiry/logout; inference-token versus management-session separation; token boundaries; two API tokens with distinct per-token metrics/statistics; cross-account isolation on every resource family; runtime registration/configuration across independently constructed app instances; both routing strategies and quota exclusion; fake upstream login success/pending/expiry/error/state mismatch/replay/concurrent completion; cancellation and credential persistence; existing chat/Responses/tool/accounting tests. Run formatting, locked Clippy with warnings denied, chart checks, and relevant full tests. Use no real credentials. Record actual measurements and limitations.

## Success criteria

A clean test deployment can issue a signup code, sign up with email/password/code, log in, issue multiple inference API tokens, register a provider, complete provider authentication via the API, set its model mapping and routing strategy, and successfully use both inference surfaces with per-token tracking. A second replica observes the same settings and login outcome. No SQL edits, Pod file copy, or restart is needed after issuing the signup code. Existing request correctness and lifecycle tests continue to pass.
