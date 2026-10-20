# Rust LLM proxy implementation report

## Outcome

Implemented a modular Rust service with API adapters, a replaceable gateway routing policy, and Codex/Claude providers. Final QA results are recorded in [qa-report.md](qa-report.md).

All 16 automated tests passed, with zero failures or ignored tests in the final complete run. Formatting and all-target Clippy checks passed. QA gives a conditional deployment pass because real subscription calls were not exercised.

The actual service also passed a loopback process smoke check covering token/CIDR authentication, disabled accounts, both API endpoints, provider statistics, metrics, JSON logs, credential exclusion, and SIGTERM shutdown. No upstream credentials or inference were used.

## Delivered behavior

- Chat Completions and Responses support collected and streamed text, native search, and client function calls/results.
- Accounts own tokens, allowed CIDRs, and multiple provider instances, including instances of the same type. Routing considers only the requesting account's configured providers, exact model aliases, provider quota, and recorded token usage.
- Codex uses direct device OAuth, refresh, and HTTP inference. Claude uses an isolated `claude -p` process with client tools handed back before execution.
- PostgreSQL stores authentication, provider credentials/quota, and request metadata/token counts. Provider statistics derive from request records. Bodies remain outside database history.
- JSON stdout includes request/response and operational logs; Prometheus exposes request outcomes, active requests, failures, tokens, and latency.
- Request bounds, cancellation, timeout, interrupted-request reconciliation, and graceful task/credential-refresh draining are implemented.
- Setup, manual SQL provisioning, configuration, and test commands are documented in [README.md](../../../README.md).

## Review corrections

Review and protocol testing corrected refresh cancellation/shutdown races, search-history replay, Unicode citation offsets, error-log correlation, and Claude parallel-call completion/result-history handling. The installed Claude CLI was exercised against a local API with synthetic credentials.

## Persistence and load

This is a new service, so there is no previous query baseline. Authentication performs a credential lookup. Routing reads provider usage/quota and account usage, then writes the initial request and its final outcome. Provider quota snapshots and rotated credentials add writes when received. Request indexes support account/provider aggregation and expired-running-request reconciliation. Aggregation reads request history; no load or capacity benchmark was performed. Routing is advisory and does not reserve quota.

## Validation limits

No real subscription credentials or billable upstream inference were used. Fixtures cannot establish current account entitlements, model availability, or live interoperability. Opaque Codex reasoning state is not retained across turns. Claude native context/reminders differ from a raw model API, and CLI protocol changes may require adapter updates. The subsequent container deployment work added PostgreSQL row-lock serialization for shared Codex refresh and Claude invocation credentials, enabling multiple replicas; see [deployment review](../20261020-container-deployment/implementation-review.md).
