# QA Report

## Verdict

**CONDITIONAL PASS for deployment.** Automated tests and local process verification pass with no known blocking defect in the tested scope. Live Codex/Claude subscription acceptance has not been verified, so this report does not claim production readiness for every upstream account or model.

## Executed checks

| Check | Observed result |
|---|---|
| `cargo fmt --check` | Passed |
| `cargo clippy --all-targets -- -D warnings` | Passed |
| `cargo test --locked` | 6 passed; 10 explicitly ignored for external test prerequisites; 0 failed |
| `cargo test --locked -- --include-ignored` with PostgreSQL and a Claude executable configured | 16 passed; 0 ignored; 0 failed |
| Compiled service process smoke over real loopback TCP | Passed |

The full run comprised 7 API integration tests, 5 Claude provider tests, 2 Codex provider tests, 1 configuration test, and 1 gateway/PostgreSQL integration test. Library, binary, and documentation targets contained no additional tests.

PostgreSQL tests created independent schemas and seeded accounts, credentials, and provider ownership directly. The actual Claude executable test used Claude Code 2.1.302 against a fake local Messages API with a test-only fake API key. It did not use a subscription account or send inference to an external service.

## Requirement and coverage mapping

| Requirement | Verification |
|---|---|
| Separate API, gateway, and provider responsibilities; replaceable routing/provider interfaces | Source review of composition, normalized contracts, registered providers, and account-scoped policy inputs |
| Both Chat Completions and Responses, collected and streaming | Real router/gateway/PostgreSQL tests assert text, usage, stable IDs, event ordering, terminal status, and errors |
| Client-defined functions and results | Both API protocols round-trip returned function calls and client results; provider fixtures inspect wire formats; actual Claude CLI returns two parallel calls and resumes their results |
| Native web search and reusable history | API search/citation tests replay exact returned assistant/output items; Codex events/citations and Claude search handoff are covered by provider fixtures |
| Accounts with multiple tokens and CIDRs | Authentication, revocation, account disabling, shared usage, IPv4-mapped peers, longest-prefix matching, and ignored forwarded headers are exercised |
| Multiple account-owned instances of the same provider type | Configuration accepts repeated provider kinds with distinct IDs; gateway integration checks ownership isolation and multiple provider instances |
| Per-account/per-provider statistics | PostgreSQL integration and process smoke inspect derived `provider_statistics` counts and token totals; missing usage remains distinct |
| Model/quota/usage routing | Gateway tests verify exact alias selection, deterministic least-usage choice, selection changes after completion, independent reset windows, exhausted/stale/unknown quota, and foreign-account exclusion |
| Direct Codex subscription authentication | Local HTTP tests verify device polling, code exchange, refresh rotation, one refresh under concurrency, persistence through cancellation, and HTTP 401/429 classification |
| Claude print-mode isolation | Subprocess tests inspect flags, history, controlled environment, MCP function handoff, successful result/exit requirements, malformed/truncated output, and process termination |
| Persistent request lifecycle | Real PostgreSQL tests cover concurrent usage, setup cancellation, timeout, partial usage on failure, failed insertion/finalization, reconciliation, and bounded output |
| JSON stdout request/response logs | Capture test checks original JSON request bodies, exact collected/SSE payloads, account/request correlation including accepted errors, and exclusion of raw authentication headers/tokens |
| Metadata-only database history | Migration/source review confirms no prompt, response, function-definition, or tool-result body columns or separate audit ledger |
| Prometheus and process operation | Router tests verify metrics; compiled-service smoke checks liveness, metrics, startup, and graceful SIGTERM exit |

## Process smoke evidence

The compiled binary was exercised with PostgreSQL and account-owned provider bindings over actual loopback TCP. Liveness succeeded; unauthenticated access returned 401; a configured direct-peer CIDR and a bearer token both authenticated; a supplied invalid token did not fall back to CIDR authentication; disabling the account rejected access. Both inference endpoints reached the account's Codex provider and returned 502 because no upstream credentials were provisioned. Two failed request records and `provider_statistics.request_count = 2` were observed for that account/provider. Metrics and JSON stdout were present, raw bearer tokens were absent, and SIGTERM ended the process with exit code 0 and empty stderr. No upstream inference request was made in this smoke test.

## Findings resolved during review

- API-generated search history originally failed replay. Typed assistant citation metadata and completed search trace items are now accepted; arbitrary executable history items remain rejected.
- Accepted provider failures before SSE headers or during collection originally lacked log correlation. Their error bodies now carry the same request/account IDs as the accepted request.
- Actual Claude continuation required preserving client results in resumed history rather than resubmitting them as a fresh prompt. The native interrupted-turn resume path now passes the real-executable/local-API roundtrip test.
- An obsolete subprocess fixture stalled during that resume change; it was replaced to match the verified protocol. The final default and complete suites pass.
- Scoped log capture missed an error event under concurrent integration testing. The capture test now uses one process-lifetime subscriber and account-specific assertions, matching production logger lifetime without serializing the suite. The final concurrent full suite passes.
- README test commands now distinguish PostgreSQL prerequisites from the optional actual-Claude-executable fixture. There are no silent prerequisite skips reported as successful tests.

Tests assert observable API, subprocess, transport, and database behavior. Expected values are controlled fixture results, not copies of implementation logic. Test schemas isolate concurrent runs; no production API or derive was added solely to simplify testing.

## Remaining limits

- No real Codex or Claude subscription login/inference was performed. Verify each configured subscription and model with text, search, and a function-result continuation before deployment.
- Opaque reasoning items are not retained between requests. Local fixtures establish function-call transport, not equivalent reasoning context or live acceptance across every Codex reasoning model.
- Claude behavior was checked against one executable version and a fake upstream API. Managed policies and later CLI protocol changes require revalidation.
- Custom web-search location/context/domain filters, stored Responses, multimodal input, and arbitrary host tools remain outside the implemented contract and are rejected.
- No load benchmark, long-running fault campaign, or multi-instance refresh test was performed. Deployment assumes one running service owns each upstream credential, and routing decisions are advisory rather than hard quota reservations.
